// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_task::{KarsTaskSpec, TaskBlueprint, TaskExecution, TaskModel};
use serde_json::json;

#[test]
fn execution_plan_roundtrips_and_every_reviewed_field_changes_authority() {
    let plan = contract::tests::plan();
    let spec = KarsTaskSpec {
        objective: "Draft handbook".into(),
        blueprint: Some(TaskBlueprint {
            execution_plan: Some(plan.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let model = TaskModel {
        provider: "azure-foundry".into(),
        deployment: "reviewed-model".into(),
    };
    let original = serde_json::to_value(&spec).unwrap();
    let roundtrip: KarsTaskSpec = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(
        roundtrip.blueprint.unwrap().execution_plan,
        Some(plan.clone())
    );
    assert_eq!(
        crate::kars_task::blueprint::effective_blueprint_with_model(&spec, &model).execution_plan,
        Some(plan)
    );
    assert_eq!(
        spec.authorization_configuration_with_model(&model)["blueprint"]["executionPlan"],
        original["blueprint"]["executionPlan"]
    );
    let digest = spec.authorization_digest_with_model(&model);
    for (pointer, value) in [
        ("schema", json!("future-version")),
        ("maxParallel", json!(2)),
        ("roles/0/name", json!("reviewer")),
        (
            "roles/0/objective",
            json!("A different reviewed objective."),
        ),
        ("roles/0/dependsOn", json!(["reviewer"])),
        ("roles/0/budgetTokens", json!(600000)),
        ("roles/0/phases/0/name", json!("review")),
        (
            "roles/0/phases/0/objective",
            json!("Another phase with a different objective."),
        ),
        ("roles/0/phases/0/capabilities", json!(["filesystem-read"])),
        ("roles/0/phases/0/minToolCalls", json!(2)),
        ("roles/0/phases/0/maxToolCalls", json!(4)),
        ("roles/0/phases/0/freshContext", json!(false)),
        (
            "roles/0/phases/0/requiredToolCalls/0/name",
            json!("other-tool"),
        ),
        (
            "roles/0/phases/0/requiredToolCalls/0/arguments/owner",
            json!("another-owner"),
        ),
        (
            "synthesis/objective",
            json!("A different synthesis objective."),
        ),
        ("synthesis/capabilities", json!(["filesystem-write"])),
        ("synthesis/maxToolCalls", json!(1)),
        ("deliverables/0/name", json!("another.md")),
        ("deliverables/0/mediaType", json!("text/plain")),
    ] {
        let mut changed = original.clone();
        *changed
            .pointer_mut(&format!("/blueprint/executionPlan/{pointer}"))
            .unwrap() = value;
        let changed: KarsTaskSpec = serde_json::from_value(changed).unwrap();
        assert_ne!(
            digest,
            changed.authorization_digest_with_model(&model),
            "{pointer}"
        );
    }
}

#[test]
fn execution_plan_contract_blocks_activation_but_preserves_legacy_tasks() {
    let mut spec = KarsTaskSpec {
        blueprint: Some(TaskBlueprint {
            execution_plan: Some(contract::tests::plan()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(crate::kars_task::validate_execution_contract(&spec).is_ok());
    spec.execution = Some(TaskExecution {
        launch: true,
        ..Default::default()
    });
    assert!(
        crate::kars_task::validate_execution_contract(&spec)
            .unwrap_err()
            .contains("TypedPlanExecutionUnavailable")
    );
    spec.blueprint.as_mut().unwrap().execution_plan = None;
    assert!(crate::kars_task::validate_execution_contract(&spec).is_ok());
    assert!(
        serde_json::to_value(spec).unwrap()["blueprint"]
            .get("executionPlan")
            .is_none()
    );
}

#[test]
fn execution_plan_survives_signed_receipt_and_requires_fresh_authority() {
    use crate::kars_receipt::{PredicateCompleteness, build_spec, build_statement, canonical_json};
    use crate::kars_task::{KarsTask, KarsTaskStatus};
    use crate::providers::signing::{DSSE_PAYLOAD_TYPE, ReceiptSigner, content_digest};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let plan = contract::tests::plan();
    let mut task = KarsTask::new(
        "reviewed-draft",
        KarsTaskSpec {
            objective: "Write the reviewed handbook without launching it.".into(),
            blueprint: Some(TaskBlueprint {
                execution_plan: Some(plan.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let status = KarsTaskStatus {
        envelope_digest: Some(task.envelope_digest()),
        ..Default::default()
    };
    let signer = ReceiptSigner::from_bytes(&[7; 32]);
    let statement = build_statement(
        &task,
        &status,
        &signer.key_id,
        &[],
        PredicateCompleteness::default(),
    )
    .unwrap();
    let bytes = canonical_json(&statement);
    let receipt = build_spec(
        "reviewed-draft",
        &task.envelope_digest(),
        &signer.key_id,
        signer.sign_statement(&bytes),
        statement.predicate.claims.clone(),
    );
    assert_eq!(receipt.dsse.payload_type, DSSE_PAYLOAD_TYPE);
    assert_eq!(receipt.dsse.signatures.len(), 1);
    assert_eq!(receipt.dsse.signatures[0].keyid, signer.key_id);
    let decoded = STANDARD.decode(&receipt.dsse.payload).unwrap();
    assert_eq!(decoded, bytes);
    let payload: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    let package = &payload["predicate"]["launchPackage"];
    let expected = serde_json::to_value(&plan).unwrap();
    assert_eq!(
        package["configuration"]["blueprint"]["executionPlan"],
        expected
    );
    assert_eq!(package["effectiveBlueprint"]["executionPlan"], expected);
    let mut binding = json!({"configuration":package["configuration"], "effectiveBlueprint":package["effectiveBlueprint"]});
    binding.sort_all_objects();
    assert_eq!(
        package["digest"],
        content_digest(&serde_json::to_vec(&binding).unwrap())
    );

    task.spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap()
        .deliverables[0]
        .name = "revised-handbook.md".into();
    assert!(
        build_statement(
            &task,
            &status,
            &signer.key_id,
            &[],
            PredicateCompleteness::default()
        )
        .is_none()
    );
    let refreshed = KarsTaskStatus {
        envelope_digest: Some(task.envelope_digest()),
        ..status
    };
    let changed = build_statement(
        &task,
        &refreshed,
        &signer.key_id,
        &[],
        PredicateCompleteness::default(),
    )
    .unwrap();
    let changed_bytes = canonical_json(&changed);
    let changed_payload = serde_json::to_value(&changed).unwrap();
    assert_ne!(bytes, changed_bytes);
    assert_ne!(
        package["digest"],
        changed_payload["predicate"]["launchPackage"]["digest"]
    );
    assert_ne!(
        receipt.dsse.signatures[0].sig,
        signer.sign_statement(&changed_bytes).signatures[0].sig
    );
}

#[test]
fn execution_plan_is_bounded_in_generated_task_and_team_schemas() {
    use kube::CustomResourceExt;
    for crd in [
        crate::kars_task::KarsTask::crd(),
        crate::kars_team::KarsTeam::crd(),
    ] {
        fn inspect(value: &serde_json::Value, count: &mut usize) {
            if let Some(plan) = value
                .get("properties")
                .and_then(|properties| properties.get("executionPlan"))
            {
                *count += 1;
                assert_eq!(plan["properties"]["roles"]["maxItems"], 8);
                assert_eq!(plan["properties"]["deliverables"]["maxItems"], 16);
                assert_eq!(
                    plan["properties"]["roles"]["items"]["properties"]["phases"]["maxItems"],
                    8
                );
                assert!(
                    !plan
                        .to_string()
                        .contains("x-kubernetes-preserve-unknown-fields")
                );
            }
            match value {
                serde_json::Value::Object(fields) => {
                    fields.values().for_each(|value| inspect(value, count))
                }
                serde_json::Value::Array(values) => {
                    values.iter().for_each(|value| inspect(value, count))
                }
                _ => (),
            }
        }
        let mut count = 0;
        inspect(&serde_json::to_value(crd).unwrap(), &mut count);
        assert!(count > 0);
    }
}
