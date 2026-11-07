// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

fn principal(role: &str) -> Principal {
    Principal {
        sub: "oidc-subject".into(),
        name: "reviewer".into(),
        roles: vec![role.into()],
    }
}

fn task() -> KarsTask {
    serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"qualification-example", "namespace":"kars-system", "uid":"original-uid"},
        "spec": {
            "objective":"Independently verify the configured provider using a bounded file task.",
            "envelope":{"tier":2,"authorityCeiling":2,"delegationDepth":0,
                "budget":{"scope":"GovernedInference","tokens":10000},
                "toolPolicyRef":{"name":"kars-default"}},
            "blueprint":{"runtime":"Hermes","model":{"provider":"azure-openai","deployment":"model-a"},
                "instructions":"Produce a useful independently verifiable answer.", "egressMode":"Strict"}
        }
    })).unwrap()
}

fn at() -> DateTime<Utc> {
    "2026-10-16T06:00:00Z".parse().unwrap()
}

fn authorization(task: &KarsTask) -> QualificationRun {
    QualificationRun {
        approved_by: "reviewer".into(),
        approved_by_sub: "oidc-subject".into(),
        approved_at: at(),
        expires_at: at() + chrono::Duration::minutes(45),
        package_sha256: package_sha256(task),
        resource_bindings: Default::default(),
    }
}

#[test]
fn only_operator_or_admin_can_authorize_qualification() {
    for role in ["operator", "admin"] {
        assert!(crate::auth::require_operator(&principal(role)).is_ok());
    }
    for role in ["user", "auditor", "Operator", "", "cluster-admin"] {
        assert!(crate::auth::require_operator(&principal(role)).is_err());
    }
}

#[test]
fn designation_waives_missing_generic_evidence_not_errors_or_ordinary_admission() {
    assert_eq!(admit(Ok(false), false), Ok(false));
    assert_eq!(admit(Ok(false), true), Ok(true));
    for marked in [false, true] {
        assert_eq!(admit(Ok(true), marked), Ok(true));
        assert_eq!(
            admit(Err("invalid evidence".into()), marked),
            Err("invalid evidence".into())
        );
    }
}

#[test]
fn qualification_requires_explicit_route_and_finite_positive_budget() {
    let mut blueprint: BlueprintDto = serde_json::from_value(json!({
        "runtime":"Hermes", "model":{"provider":"azure-openai","deployment":"model-a"}
    }))
    .unwrap();
    assert!(validate_package(&blueprint, Some(10000)).is_ok());
    for budget in [None, Some(0), Some(-1)] {
        assert!(validate_package(&blueprint, budget).is_err());
    }
    blueprint.model = None;
    assert!(validate_package(&blueprint, Some(10000)).is_err());
    blueprint.runtime = None;
    assert!(validate_package(&blueprint, Some(10000)).is_err());
}

#[test]
fn strict_egress_binding_survives_crd_pruning_but_preserves_reviewed_destinations() {
    for mode in ["strict", "Strict"] {
        let mut submitted = task();
        submitted.spec.blueprint.as_mut().unwrap().egress_mode = Some(mode.into());
        let record = authorization(&submitted);
        let mut persisted = serde_json::to_value(&submitted).unwrap();
        persisted["spec"]["blueprint"]
            .as_object_mut()
            .unwrap()
            .remove("egressMode");
        let persisted: KarsTask = serde_json::from_value(persisted).unwrap();
        assert!(validate_binding(&persisted, &record, at()).is_ok());
        let mut changed = persisted.clone();
        changed.spec.blueprint.as_mut().unwrap().egress_mode = Some("learn".into());
        assert!(validate_binding(&changed, &record, at()).is_err());
        let mut changed = serde_json::to_value(&persisted).unwrap();
        changed["spec"]["blueprint"]["egress"] = json!([{"host":"other.example","port":443}]);
        assert!(
            validate_binding(&serde_json::from_value(changed).unwrap(), &record, at()).is_err()
        );
    }
}

#[test]
fn qualification_rejects_unsupported_egress_modes_instead_of_silently_dropping_them() {
    let mut blueprint: BlueprintDto = serde_json::from_value(json!({
        "runtime":"Hermes", "model":{"provider":"azure-openai","deployment":"model-a"}
    }))
    .unwrap();
    for mode in [None, Some("strict"), Some("Strict")] {
        blueprint.egress_mode = mode.map(str::to_string);
        assert!(validate_package(&blueprint, Some(10000)).is_ok());
    }
    for mode in ["learn", "Learn", "", "unknown"] {
        blueprint.egress_mode = Some(mode.into());
        assert!(validate_package(&blueprint, Some(10000)).is_err());
    }
}

#[test]
fn deadline_is_absolute_and_bounded_with_no_clock_based_renewal() {
    let task = task();
    let mut record = authorization(&task);
    assert!(validate_binding(&task, &record, at()).is_ok());
    assert!(validate_binding(&task, &record, at() + chrono::Duration::minutes(44)).is_ok());
    assert!(validate_binding(&task, &record, record.expires_at).is_err());
    assert!(validate_binding(&task, &record, at() - chrono::Duration::seconds(1)).is_err());
    record.expires_at += chrono::Duration::seconds(1);
    assert!(validate_binding(&task, &record, at()).is_err());
    record.expires_at = record.approved_at;
    assert!(validate_binding(&task, &record, at()).is_err());
}

#[test]
fn authorization_binds_name_namespace_objective_envelope_and_all_reviewed_blueprint_fields() {
    let original = task();
    let record = authorization(&original);
    for (pointer, value) in [
        ("/metadata/name", json!("clone")),
        ("/metadata/namespace", json!("other-workspace")),
        ("/spec/objective", json!("Different work")),
        ("/spec/envelope/budget/tokens", json!(20000)),
        ("/spec/envelope/tier", json!(3)),
        ("/spec/blueprint/runtime", json!("OpenClaw")),
        ("/spec/blueprint/model/provider", json!("azure-foundry")),
        ("/spec/blueprint/model/deployment", json!("other-model")),
        (
            "/spec/blueprint/instructions",
            json!("Different instructions"),
        ),
        ("/spec/blueprint/egressMode", json!("Learn")),
    ] {
        let mut changed = serde_json::to_value(&original).unwrap();
        *changed.pointer_mut(pointer).unwrap() = value;
        let changed = serde_json::from_value(changed).unwrap();
        assert!(
            validate_binding(&changed, &record, at()).is_err(),
            "{pointer}"
        );
    }
    let mut changed = serde_json::to_value(&original).unwrap();
    changed["spec"]["blueprint"]["modelFallbacks"] =
        json!([{"provider":"azure-foundry","deployment":"other"}]);
    let changed = serde_json::from_value(changed).unwrap();
    assert!(validate_binding(&changed, &record, at()).is_err());
}

#[test]
fn runtime_status_launch_gate_and_unrelated_metadata_do_not_rewrite_reviewed_package() {
    let mut task = task();
    let record = authorization(&task);
    task.metadata.resource_version = Some("new-rv".into());
    task.metadata
        .annotations
        .get_or_insert_with(Default::default)
        .insert("kars.azure.com/run-requested".into(), "nonce".into());
    task.spec.execution = Some(crate::kars::task::TaskExecution {
        launch: true,
        runtime: None,
    });
    assert!(validate_binding(&task, &record, at()).is_ok());
}

#[test]
fn malformed_markers_fail_closed_and_cannot_be_inherited() {
    let mut task = task();
    assert!(record(&task).unwrap().is_none());
    assert!(forbid_inherited_run(&task).is_ok());
    task.metadata
        .annotations
        .get_or_insert_with(Default::default)
        .insert(ANNOTATION.into(), "{}".into());
    assert!(record(&task).is_err());
    assert!(forbid_inherited_run(&task).is_err());
}

#[test]
fn qualification_request_defaults_off_and_requires_a_boolean() {
    let compose: crate::routes::compose::ComposeRequest = serde_json::from_value(json!({
        "objective":"test", "spending":{"budget_tokens":null,"role_tokens":{}}
    }))
    .unwrap();
    assert!(!compose.qualification_run);
    let validate: crate::routes::validate::ValidateRequest =
        serde_json::from_value(json!({})).unwrap();
    assert!(!validate.qualification_run);
    assert!(
        serde_json::from_value::<crate::routes::compose::ComposeRequest>(
            json!({"objective":"test","qualification_run":"true", "spending":{"budget_tokens":null,"role_tokens":{}}})
        )
        .is_err()
    );
}

#[test]
fn public_qualification_path_never_promotes_routes_or_resources_and_reuses_only_pending_runs() {
    const ENV: &str = "KARS_PER_RUN_QUALIFICATION_TEST";
    if std::env::var_os(ENV).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "routes::qualification_runs::tests::public_qualification_path_never_promotes_routes_or_resources_and_reuses_only_pending_runs", "--nocapture"])
            .env(ENV, "1")
            .env("BRIDGE_ROUTE_QUALIFICATION_MODE", "required")
            .env("BRIDGE_QUALIFICATION_RECORDS_JSON", "[]")
            .env_remove("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let required = ["single-agent".into(), "telemetry".into()]
        .into_iter()
        .collect();
    let normal = crate::routes::options::route_admitted(
        "Hermes",
        "azure-openai",
        "model-a",
        &required,
        1,
        Some(10000),
    );
    assert_eq!(normal, Ok(false));
    assert_eq!(admit(normal, true), Ok(true));
    assert_eq!(
        crate::routes::options::route_qualification(
            "Hermes",
            "azure-openai",
            "model-a",
            &required,
            1,
            Some(10000)
        ),
        Ok(false)
    );
    let mcp = crate::routes::options::RefOption {
        name: "service".into(),
        namespace: "kars-system".into(),
        summary: None,
        mode: None,
        discovered_tools: vec![],
        tool_schema_digest: Some("sha256:current".into()),
        compiled_digest: None,
        backend: None,
        readiness: None,
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: vec![],
    };
    assert!(
        !crate::routes::options::mcp_server_admitted_for_route(
            "Hermes",
            "azure-openai",
            "model-a",
            &mcp
        )
        .unwrap()
    );
    let mut task = task();
    stamp(&mut task, &principal("operator"), Default::default()).unwrap();
    let stamped = record(&task).unwrap().unwrap();
    assert_eq!(stamped.approved_by_sub, "oidc-subject");
    assert_eq!(
        stamped.expires_at - stamped.approved_at,
        chrono::Duration::minutes(45)
    );
    assert!(authorize_stored(&task, &principal("user")).is_err());
    assert!(authorize_stored(&task, &principal("admin")).unwrap());
    let mut expired = stamped;
    expired.approved_at -= chrono::Duration::hours(1);
    expired.expires_at -= chrono::Duration::hours(1);
    task.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(ANNOTATION.into(), serde_json::to_string(&expired).unwrap());
    assert!(authorize_run(&task, &principal("operator")).is_err());
    task.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert("kars.azure.com/run-requested".into(), "first-run".into());
    assert!(authorize_run(&task, &principal("operator")).unwrap());
    task.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert("kars.azure.com/run-completed".into(), "first-run".into());
    assert!(authorize_run(&task, &principal("operator")).is_err());
}
