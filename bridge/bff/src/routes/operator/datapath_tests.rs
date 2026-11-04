// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::{ServiceExt, service_fn};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-15T15:00:30Z")
        .unwrap()
        .into()
}

fn cm(name: &str, key: &str, value: Value) -> ConfigMap {
    serde_json::from_value(json!({
        "apiVersion":"v1","kind":"ConfigMap",
        "metadata": {
            "name":name,"namespace":"kars-system","uid":"report-uid","resourceVersion":"1",
            "labels":{"app.kubernetes.io/managed-by":"Helm","kars.azure.com/witness-addon":"true"},
            "annotations":{"meta.helm.sh/release-name":"kars-datapath-witness","meta.helm.sh/release-namespace":"kars-system"}
        },
        "data": {key: value.to_string()}
    })).unwrap()
}

fn settings(enabled: bool) -> ConfigMap {
    settings_for(enabled, &["demo"])
}

fn settings_for(enabled: bool, sandboxes: &[&str]) -> ConfigMap {
    cm(
        "kars-datapath-witness-settings",
        "settings.json",
        json!({
            "schema_version":1,"enabled":enabled,"release_revision":2,
            "config_digest":"a".repeat(64),"sandboxes":sandboxes
        }),
    )
}

fn report() -> Value {
    json!({
        "schema_version":1,"status":"observed","generated_at":"2026-09-15T15:00:30Z",
        "started_at":"2026-09-15T15:00:15Z","window_seconds":15,
        "release_revision":2,"config_digest":"a".repeat(64),"publisher_uid":"report-uid",
        "coverage":"partial","event_count":1,"nodes_targeted":["node-1"],"nodes_with_events":["node-1"],
        "sandboxes":[{
            "namespace":"kars-demo","sandbox":"demo","egress_mode":"Strict",
            "declared_hosts":[],"observed_dns":["example.com"],"observed_connects":0,
            "beyond_declared":["example.com"],"unused_declared":[],"verdict":"BEYOND-DECLARED"
        }]
    })
}

fn scoped_settings(scope: Value, sandboxes: Value) -> ConfigMap {
    cm(
        "kars-datapath-witness-settings",
        "settings.json",
        json!({
            "schema_version":1,"enabled":true,"release_revision":2,
            "config_digest":"a".repeat(64),"scope":scope,"sandboxes":sandboxes
        }),
    )
}

fn all_report() -> Value {
    let mut value = report();
    value["scope"] = json!("all");
    value["sandboxes"][0]["sandbox_uid"] = json!("sandbox-cr-uid");
    value
}

fn classify_all_report(value: Value) -> DatapathWitnessDto {
    classify(
        Ok(Some(scoped_settings(json!("all"), json!([])))),
        Ok(Some(cm("kars-datapath-witness", "witness.json", value))),
        now(),
    )
}

fn classify_report(value: Value) -> DatapathWitnessDto {
    classify(
        Ok(Some(settings(true))),
        Ok(Some(cm("kars-datapath-witness", "witness.json", value))),
        now(),
    )
}

fn assert_invalid_without_evidence(dto: DatapathWitnessDto) {
    assert_eq!(dto.state, "invalid");
    assert!(!dto.enabled);
    assert!(dto.sandboxes.is_empty());
}

fn with_owner(mut cm: ConfigMap, owner: &str) -> ConfigMap {
    cm.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert("meta.helm.sh/release-name".into(), owner.into());
    cm
}

#[test]
fn witness_scope_defaults_to_selected_and_requires_matching_intent() {
    let legacy = classify_report(report());
    assert_eq!(legacy.scope.as_deref(), Some("selected"));
    for explicit_settings in [false, true] {
        for explicit_report in [false, true] {
            let settings = if explicit_settings {
                scoped_settings(json!("selected"), json!(["demo"]))
            } else {
                settings(true)
            };
            let mut value = report();
            if explicit_report {
                value["scope"] = json!("selected");
            }
            assert_eq!(
                classify(
                    Ok(Some(settings)),
                    Ok(Some(cm("kars-datapath-witness", "witness.json", value))),
                    now(),
                )
                .state,
                "observed"
            );
        }
    }
    assert_invalid_without_evidence(classify_report(all_report()));
    assert_invalid_without_evidence(classify_all_report(report()));
    for scope in [json!("selected"), json!("fleet"), json!(null)] {
        let mut value = all_report();
        value["scope"] = scope;
        assert_invalid_without_evidence(classify_all_report(value));
    }
    let mut different_digest = all_report();
    different_digest["config_digest"] = json!("b".repeat(64));
    assert_invalid_without_evidence(classify_all_report(different_digest));
    let missing = classify(Ok(None), Ok(None), now());
    assert_eq!(missing.scope, None);
    let pending = classify(
        Ok(Some(scoped_settings(json!("all"), json!([])))),
        Ok(None),
        now(),
    );
    assert_eq!(pending.scope.as_deref(), Some("all"));
    assert_eq!(pending.state, "pending");
}

#[test]
fn witness_settings_reject_ambiguous_scope_and_preserve_selected_limits() {
    for (scope, names) in [
        (json!("all"), json!(["demo"])),
        (json!("fleet"), json!([])),
        (json!(null), json!([])),
        (json!("selected"), json!([])),
        (json!("selected"), json!(["demo", "demo"])),
        (json!("selected"), json!(["INVALID"])),
        (
            json!("selected"),
            json!((0..51).map(|i| format!("worker-{i}")).collect::<Vec<_>>()),
        ),
    ] {
        let dto = classify(Ok(Some(scoped_settings(scope, names))), Ok(None), now());
        assert_eq!(dto.settings_state, "invalid");
        assert_eq!(dto.scope, None);
        assert_invalid_without_evidence(dto);
    }
    let names = (0..50).map(|i| format!("worker-{i}")).collect::<Vec<_>>();
    let dto = classify(
        Ok(Some(scoped_settings(json!("selected"), json!(names)))),
        Ok(None),
        now(),
    );
    assert_eq!(dto.settings_state, "known");
    assert_eq!(dto.state, "pending");
    let mut wrong_name = report();
    wrong_name["sandboxes"][0]["sandbox"] = json!("another");
    wrong_name["sandboxes"][0]["namespace"] = json!("kars-another");
    assert_invalid_without_evidence(classify_report(wrong_name));
}

#[test]
fn witness_all_scope_has_no_fifty_row_cap_and_does_not_claim_inventory_coverage() {
    let mut value = all_report();
    value["event_count"] = json!(60);
    value["sandboxes"] = json!(
        (0..60)
            .map(|i| {
                let mut row = all_report()["sandboxes"][0].clone();
                row["sandbox"] = json!(format!("worker-{i}"));
                row["namespace"] = json!(format!("kars-worker-{i}"));
                row["sandbox_uid"] = json!(format!("cr-uid-{i}"));
                row
            })
            .collect::<Vec<_>>()
    );
    let dto = classify_all_report(value.clone());
    assert_eq!(dto.state, "observed");
    assert_eq!(dto.scope.as_deref(), Some("all"));
    assert_eq!(dto.coverage, "partial");
    assert_eq!(dto.installation_state, "unknown");
    assert_eq!(dto.sandboxes.len(), 60);
    value["sandboxes"] = json!([]);
    value["status"] = json!("empty");
    value["event_count"] = json!(0);
    value["nodes_with_events"] = json!([]);
    let empty = classify_all_report(value.clone());
    assert_eq!(empty.state, "empty");
    assert_eq!(empty.coverage, "partial");
    assert!(!empty.enabled);
    value["padding"] = json!("x".repeat(MAX_BODY_BYTES));
    assert_invalid_without_evidence(classify_all_report(value));
}

#[test]
fn witness_all_rows_require_valid_unique_names_namespaces_and_cr_identity() {
    for (field, bad) in [
        ("sandbox", json!("")),
        ("sandbox", json!("Uppercase")),
        ("sandbox", json!("bad.name")),
        ("sandbox", json!("-bad")),
        ("sandbox", json!("bad-")),
        ("sandbox", json!("a".repeat(59))),
        ("namespace", json!("kars-something-else")),
        ("sandbox_uid", json!(null)),
        ("sandbox_uid", json!("")),
        ("sandbox_uid", json!(" ")),
    ] {
        let mut value = all_report();
        value["sandboxes"][0][field] = bad;
        assert_invalid_without_evidence(classify_all_report(value));
    }
    let mut missing_uid = all_report();
    missing_uid["sandboxes"][0]
        .as_object_mut()
        .unwrap()
        .remove("sandbox_uid");
    assert_invalid_without_evidence(classify_all_report(missing_uid));
    let mut duplicate = all_report();
    duplicate["sandboxes"] = json!([duplicate["sandboxes"][0], duplicate["sandboxes"][0]]);
    assert_invalid_without_evidence(classify_all_report(duplicate));
}

#[test]
fn witness_unavailable_baselines_preserve_observations_without_comparisons() {
    for diagnostic in ["baseline_unavailable", "declaration_invalid"] {
        let mut value = all_report();
        value["event_count"] = json!(3);
        value["sandboxes"][0]["observed_connects"] = json!(2);
        value["sandboxes"][0]["egress_mode"] = json!(null);
        value["sandboxes"][0]["verdict"] = json!("UNAVAILABLE");
        value["sandboxes"][0]["declaration_diagnostic"] = json!(diagnostic);
        value["sandboxes"][0]["beyond_declared"] = json!([]);
        let dto = classify_all_report(value.clone());
        assert_eq!(dto.state, "observed");
        assert_eq!(dto.sandboxes[0].observed_connects, 2);
        assert_eq!(dto.sandboxes[0].observed_dns, ["example.com"]);
        assert_eq!(
            dto.sandboxes[0].declaration_diagnostic.as_deref(),
            Some(diagnostic)
        );
        assert_eq!(dto.sandboxes[0].egress_mode, None);
        for (field, bad) in [
            ("egress_mode", json!("Learn")),
            ("egress_mode", json!("Strict")),
            (
                "declaration_diagnostic",
                json!("arbitrary upstream details"),
            ),
            ("declaration_diagnostic", json!(null)),
            ("declaration_diagnostic", json!("")),
            ("verdict", json!("NO-TRAFFIC")),
            ("declared_hosts", json!(["example.com"])),
            ("beyond_declared", json!(["example.com"])),
            ("unused_declared", json!(["example.com"])),
            ("observed_connects", json!(4)),
            ("observed_dns", json!(["example.com", "example.com"])),
        ] {
            let mut invalid = value.clone();
            invalid["sandboxes"][0][field] = bad;
            assert_invalid_without_evidence(classify_all_report(invalid));
        }
        for field in ["egress_mode", "declaration_diagnostic"] {
            let mut invalid = value.clone();
            invalid["sandboxes"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert_invalid_without_evidence(classify_all_report(invalid));
        }
        value["status"] = json!("empty");
        value["event_count"] = json!(0);
        value["nodes_with_events"] = json!([]);
        value["sandboxes"][0]["observed_connects"] = json!(0);
        value["sandboxes"][0]["observed_dns"] = json!([]);
        assert_eq!(classify_all_report(value).state, "empty");
    }
    for diagnostic in [
        json!("baseline_unavailable"),
        json!("declaration_invalid"),
        json!(null),
    ] {
        let mut value = all_report();
        value["sandboxes"][0]["declaration_diagnostic"] = diagnostic;
        assert_invalid_without_evidence(classify_all_report(value));
    }
    let dto = classify_all_report(all_report());
    assert!(
        serde_json::to_value(&dto).unwrap()["sandboxes"][0]
            .get("declaration_diagnostic")
            .is_none()
    );
}

#[test]
fn witness_core_and_standalone_require_matching_owners() {
    for release in ["kars", "kars-datapath-witness"] {
        for enabled in [true, false] {
            let dto = classify(
                Ok(Some(with_owner(settings(enabled), release))),
                Ok(None),
                now(),
            );
            assert_eq!(dto.owner_release.as_deref(), Some(release));
            assert_eq!(dto.state, if enabled { "pending" } else { "disabled" });
        }
        for report_owner in ["kars", "kars-datapath-witness", "foreign"] {
            let dto = classify(
                Ok(Some(with_owner(settings(true), release))),
                Ok(Some(with_owner(
                    cm("kars-datapath-witness", "witness.json", report()),
                    report_owner,
                ))),
                now(),
            );
            if release == report_owner {
                assert_eq!(dto.state, "observed");
                assert!(dto.enabled);
                assert_eq!(dto.owner_release.as_deref(), Some(release));
            } else {
                assert_invalid_without_evidence(dto);
            }
        }
    }
    let dto = classify(
        Ok(Some(with_owner(settings(true), "foreign"))),
        Ok(Some(with_owner(
            cm("kars-datapath-witness", "witness.json", report()),
            "foreign",
        ))),
        now(),
    );
    assert!(dto.owner_release.is_none());
    assert_invalid_without_evidence(dto);
}

#[test]
fn witness_foreign_empty_report_is_not_a_pending_owned_publisher() {
    for (settings_owner, report_owner) in [
        ("kars", "kars-datapath-witness"),
        ("kars-datapath-witness", "kars"),
    ] {
        let mut empty = with_owner(
            cm("kars-datapath-witness", "witness.json", report()),
            report_owner,
        );
        empty.data = None;
        assert_invalid_without_evidence(classify(
            Ok(Some(with_owner(settings(true), settings_owner))),
            Ok(Some(empty)),
            now(),
        ));
    }
}

#[derive(Deserialize)]
struct HostMatchCase {
    name: String,
    host: String,
    declared_hosts: Vec<String>,
    matches: bool,
}

#[derive(Deserialize)]
struct ReportCase {
    name: String,
    sandbox: DatapathWitnessSandbox,
}

#[derive(Deserialize)]
struct MatchingContract {
    matching: Vec<HostMatchCase>,
    reports: Vec<ReportCase>,
    rejected_modes: Vec<String>,
}

fn matching_contract() -> MatchingContract {
    serde_json::from_str(include_str!("datapath_matching_contract.json"))
        .expect("valid shared host-matching fixture")
}

fn report_for(sandbox: &DatapathWitnessSandbox) -> Value {
    let count = sandbox.observed_dns.len() as u64 + sandbox.observed_connects;
    let mut value = report();
    value["event_count"] = json!(count);
    value["status"] = json!(if count == 0 { "empty" } else { "observed" });
    value["nodes_with_events"] = if count == 0 {
        json!([])
    } else {
        json!(["node-1"])
    };
    value["sandboxes"] = json!([sandbox]);
    value
}

#[test]
fn witness_host_matching_contract_agrees_with_actual_producer() {
    for case in matching_contract().matching {
        assert_eq!(
            declared_host(&case.host, &case.declared_hosts),
            case.matches,
            "{}",
            case.name
        );
    }
    let Ok(output) = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/routes/operator/datapath_matching_contract_test.py"
        ))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
    else {
        panic!("Python 3 must be available for the shared producer contract regression");
    };
    assert!(
        output.status.success(),
        "the actual producer must satisfy the shared matching fixture"
    );
}

#[test]
fn witness_exact_and_wildcard_sets_must_be_complete_in_every_supported_mode() {
    for case in matching_contract().reports {
        let value = report_for(&case.sandbox);
        let mut fleet_value = value.clone();
        fleet_value["scope"] = json!("all");
        fleet_value["sandboxes"][0]["sandbox_uid"] = json!("sandbox-cr-uid");
        let fleet_dto = classify_all_report(fleet_value);
        let dto = classify_report(value.clone());
        assert_eq!(fleet_dto.state, dto.state, "{}", case.name);
        assert_eq!(fleet_dto.sandboxes.len(), 1, "{}", case.name);
        assert_eq!(
            fleet_dto.sandboxes[0].verdict, case.sandbox.verdict,
            "{}",
            case.name
        );
        let observed = !case.sandbox.observed_dns.is_empty() || case.sandbox.observed_connects > 0;
        assert_eq!(
            dto.state,
            if observed { "observed" } else { "empty" },
            "{}",
            case.name
        );
        assert_eq!(dto.enabled, observed, "{}", case.name);
        assert_eq!(dto.sandboxes.len(), 1, "{}", case.name);
        assert_eq!(
            dto.sandboxes[0].beyond_declared, case.sandbox.beyond_declared,
            "{}",
            case.name
        );
        assert_eq!(
            dto.sandboxes[0].verdict, case.sandbox.verdict,
            "{}",
            case.name
        );
        for index in 0..case.sandbox.beyond_declared.len() {
            let mut incomplete = value.clone();
            incomplete["sandboxes"][0]["beyond_declared"]
                .as_array_mut()
                .unwrap()
                .remove(index);
            // Even retaining the correct verdict cannot excuse omitted hosts.
            assert_invalid_without_evidence(classify_report(incomplete.clone()));
            incomplete["sandboxes"][0]["verdict"] = json!("NO-BEYOND-OBSERVED");
            assert_invalid_without_evidence(classify_report(incomplete));
        }
        if case.sandbox.beyond_declared.len() > 1 {
            let mut reordered = value;
            reordered["sandboxes"][0]["beyond_declared"]
                .as_array_mut()
                .unwrap()
                .reverse();
            assert_eq!(classify_report(reordered).state, "observed");
        }
    }
}

#[test]
fn witness_mixed_modes_preserve_learn_semantics_and_reject_open_without_partial_evidence() {
    let mut strict = report()["sandboxes"][0].clone();
    strict["sandbox"] = json!("strict");
    strict["namespace"] = json!("kars-strict");
    let mut learn = strict.clone();
    learn["sandbox"] = json!("learn");
    learn["namespace"] = json!("kars-learn");
    learn["egress_mode"] = json!("Learn");
    learn["verdict"] = json!("LEARN");
    let mut mixed = report();
    mixed["sandboxes"] = json!([strict, learn]);
    mixed["event_count"] = json!(2);
    let classify_mixed = |value| {
        classify(
            Ok(Some(settings_for(true, &["strict", "learn"]))),
            Ok(Some(cm("kars-datapath-witness", "witness.json", value))),
            now(),
        )
    };
    let dto = classify_mixed(mixed.clone());
    assert_eq!(dto.state, "observed");
    assert!(dto.enabled);
    assert_eq!(dto.sandboxes.len(), 2);
    assert_eq!(dto.sandboxes[0].verdict, "BEYOND-DECLARED");
    assert_eq!(dto.sandboxes[1].verdict, "LEARN");
    let mut omitted_learn = mixed.clone();
    omitted_learn["sandboxes"][1]["beyond_declared"] = json!([]);
    assert_invalid_without_evidence(classify_mixed(omitted_learn));
    for mode in matching_contract().rejected_modes {
        let mut unsupported = mixed.clone();
        unsupported["sandboxes"][1]["egress_mode"] = json!(mode);
        assert_invalid_without_evidence(classify_mixed(unsupported));
    }
}

#[test]
fn witness_missing_is_unknown_not_not_installed_and_intent_is_separate() {
    let dto = classify(Ok(None), Ok(None), now());
    assert_eq!(dto.state, "missing");
    assert_eq!(dto.requested_enabled, None);
    assert_eq!(dto.installation_state, "unknown");
    assert!(!dto.enabled);
    assert!(!dto.install_hint.contains("install.sh"));
    let dto = classify(Ok(Some(settings(true))), Ok(None), now());
    assert_eq!(dto.state, "pending");
    assert_eq!(dto.requested_enabled, Some(true));
    let dto = classify(Ok(Some(settings(false))), Ok(None), now());
    assert_eq!(dto.state, "disabled");
    assert_eq!(dto.installation_state, "unknown");
}

#[test]
fn witness_fresh_and_empty_samples_never_claim_complete_coverage() {
    let dto = classify_report(report());
    assert_eq!(dto.state, "observed");
    assert!(dto.enabled);
    assert_eq!(dto.coverage, "partial");
    assert_eq!(dto.age_seconds, Some(0));
    assert_eq!(dto.sandboxes[0].verdict, "BEYOND-DECLARED");
    let mut empty = report();
    empty["status"] = json!("empty");
    empty["event_count"] = json!(0);
    empty["nodes_with_events"] = json!([]);
    empty["sandboxes"][0]["observed_dns"] = json!([]);
    empty["sandboxes"][0]["beyond_declared"] = json!([]);
    empty["sandboxes"][0]["verdict"] = json!("NO-TRAFFIC");
    let dto = classify_report(empty);
    assert_eq!(dto.state, "empty");
    assert!(!dto.enabled);
    assert_eq!(dto.coverage, "partial");
}

#[test]
fn witness_malformed_stale_future_and_identity_fail_closed() {
    // Reviewer counterexample: Strict deny-all plus observed DNS cannot be a
    // fresh negative merely because both reported verdict and beyond set lie.
    let mut omitted = report();
    omitted["sandboxes"][0]["beyond_declared"] = json!([]);
    omitted["sandboxes"][0]["verdict"] = json!("NO-BEYOND-OBSERVED");
    assert_invalid_without_evidence(classify_report(omitted));
    for (field, value) in [
        ("generated_at", json!("invalid")),
        ("generated_at", json!("2026-09-15T15:01:01Z")),
        ("schema_version", json!(2)),
        ("window_seconds", json!(0)),
        ("publisher_uid", json!("replaced-object")),
        ("release_revision", json!(1)),
        ("config_digest", json!("b".repeat(64))),
        ("coverage", json!("complete")),
        ("nodes_with_events", json!(["different-node"])),
        ("event_count", json!(0)),
        ("started_at", json!("2026-09-15T15:00:29Z")),
        ("sandboxes", json!([])),
        (
            "sandboxes",
            json!([{"sandbox":"demo","verdict":"COMPLIANT"}]),
        ),
    ] {
        let mut doc = report();
        doc[field] = value;
        let dto = classify_report(doc);
        assert_eq!(dto.state, "invalid", "{field}");
        assert!(!dto.enabled);
        assert!(dto.sandboxes.is_empty());
    }
    let mut inconsistent = report();
    inconsistent["sandboxes"][0]["verdict"] = json!("LEARN");
    assert_eq!(classify_report(inconsistent).state, "invalid");
    let mut inconsistent = report();
    inconsistent["sandboxes"][0]["beyond_declared"] = json!(["never-observed.example"]);
    assert_eq!(classify_report(inconsistent).state, "invalid");
    let mut stale = report();
    stale["generated_at"] = json!("2026-09-15T14:57:29Z");
    let dto = classify_report(stale);
    assert_eq!(dto.state, "stale");
    assert_eq!(dto.age_seconds, Some(181));
    assert!(!dto.enabled);
    assert!(dto.sandboxes.is_empty());
    let boundary = classify(
        Ok(Some(settings(true))),
        Ok(Some(cm("kars-datapath-witness", "witness.json", report()))),
        now() + chrono::Duration::seconds(180),
    );
    assert_eq!(boundary.state, "observed");
}

#[test]
fn witness_empty_configmaps_legacy_and_failed_capture_are_distinct() {
    let mut object = cm("kars-datapath-witness", "witness.json", report());
    object
        .data
        .as_mut()
        .unwrap()
        .insert("witness.json".into(), "{".into());
    assert_eq!(
        classify(Ok(Some(settings(true))), Ok(Some(object.clone())), now()).state,
        "invalid"
    );
    object.data = None;
    assert_eq!(
        classify(Ok(Some(settings(true))), Ok(Some(object.clone())), now()).state,
        "pending"
    );
    object.metadata.labels = None;
    assert_eq!(classify(Ok(None), Ok(Some(object)), now()).state, "invalid");
    let mut legacy = report();
    legacy.as_object_mut().unwrap().remove("schema_version");
    let dto = classify_report(legacy);
    assert_eq!(dto.state, "legacy");
    assert!(!dto.enabled);
    let mut failed = report();
    failed["status"] = json!("unavailable");
    failed["diagnostic"] = json!("secret should never be returned");
    failed["sandboxes"] = json!([]);
    let dto = classify_report(failed);
    assert_eq!(dto.state, "unavailable");
    assert!(
        !serde_json::to_string(&dto)
            .unwrap()
            .contains("secret should")
    );
    let mut invalid = settings(true);
    invalid
        .data
        .as_mut()
        .unwrap()
        .insert("settings.json".into(), "{}".into());
    let dto = classify(Ok(Some(invalid)), Ok(None), now());
    assert_eq!(dto.state, "invalid");
    assert_eq!(dto.requested_enabled, None);
}

#[test]
fn witness_unavailable_reasons_are_allowlisted_without_exposing_sample_rows() {
    for (code, explanation) in [
        (
            "sandbox_inventory_changed_during_capture",
            "Sandbox inventory changed",
        ),
        (
            "gadget_nodes_changed_during_capture",
            "Capture nodes changed",
        ),
        (
            "declaration_changed_during_capture",
            "Egress declarations changed",
        ),
        (
            "sample_expired_before_publication",
            "expired before publication",
        ),
        ("gadget_not_ready", "not ready on every targeted node"),
        (
            "gadget_node_set_incomplete",
            "not ready on every targeted node",
        ),
        ("capture_failed", "Capture did not complete"),
        ("capture_ended_early", "Capture did not complete"),
        ("capture_interrupted", "Capture did not complete"),
        ("capture_output_limit", "exceeded its output limit"),
        ("capture_invalid_json", "could not be validated"),
        (
            "capture_event_schema_or_attribution",
            "could not be validated",
        ),
        ("capture_diagnostics", "could not be validated"),
    ] {
        let mut value = all_report();
        value["status"] = json!("unavailable");
        value["diagnostic"] = json!(code);
        let dto = classify_all_report(value);
        assert_eq!(dto.state, "unavailable", "{code}");
        assert!(dto.diagnostic.contains(explanation), "{code}");
        assert!(!dto.enabled);
        assert!(dto.sandboxes.is_empty());
        assert!(dto.nodes_targeted.is_empty());
        assert!(dto.nodes_with_events.is_empty());
        assert_eq!(dto.event_count, None);
    }
    for diagnostic in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!("secret should never be returned")),
    ] {
        let mut value = report();
        value["status"] = json!("unavailable");
        if let Some(diagnostic) = diagnostic {
            value["diagnostic"] = diagnostic;
        }
        let dto = classify_report(value);
        assert_eq!(dto.state, "unavailable");
        assert_eq!(dto.diagnostic, unavailable_diagnostic(None));
        assert!(
            !serde_json::to_string(&dto)
                .unwrap()
                .contains("secret should")
        );
        assert!(dto.sandboxes.is_empty());
    }
}

#[test]
fn witness_known_diagnostic_never_bypasses_identity_freshness_or_scope_validation() {
    let mut value = all_report();
    value["status"] = json!("unavailable");
    value["diagnostic"] = json!("sandbox_inventory_changed_during_capture");
    for (field, invalid) in [
        ("publisher_uid", json!("other-publisher")),
        ("release_revision", json!(3)),
        ("config_digest", json!("b".repeat(64))),
        ("scope", json!("selected")),
        ("coverage", json!("complete")),
        ("schema_version", json!(2)),
        ("generated_at", json!("2026-09-15T15:01:01Z")),
        ("diagnostic", json!({"secret": "not a code"})),
    ] {
        let mut invalid_value = value.clone();
        invalid_value[field] = invalid;
        let dto = classify_all_report(invalid_value);
        assert!(
            !dto.diagnostic.contains("Sandbox inventory changed"),
            "{field}"
        );
        assert_invalid_without_evidence(dto);
    }
    value["generated_at"] = json!("2026-09-15T14:57:29Z");
    let dto = classify_all_report(value);
    assert_eq!(dto.state, "stale");
    assert!(!dto.diagnostic.contains("Sandbox inventory changed"));
    assert!(dto.sandboxes.is_empty());
}

#[tokio::test]
async fn witness_route_only_gets_two_fixed_configmaps_and_preserves_api_errors() {
    for (status, all) in [
        (200, false),
        (200, true),
        (403, true),
        (500, true),
        (0, true),
    ] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let service = service_fn(move |request: Request<_>| {
            let seen = seen.clone();
            async move {
                assert_eq!(request.method(), Method::GET);
                let path = request.uri().path().to_owned();
                seen.lock().unwrap().push(path.clone());
                let name = path
                    .strip_prefix("/api/v1/namespaces/kars-system/configmaps/")
                    .unwrap();
                assert!(
                    ["kars-datapath-witness-settings", "kars-datapath-witness"].contains(&name)
                );
                if status == 0 {
                    return Err(std::io::Error::other(
                        "arbitrary credentials or transport details",
                    ));
                }
                let value = if status == 200 {
                    if name.ends_with("settings") {
                        serde_json::to_value(if all {
                            scoped_settings(json!("all"), json!([]))
                        } else {
                            settings(true)
                        })
                        .unwrap()
                    } else {
                        let mut doc = if all { all_report() } else { report() };
                        doc["generated_at"] = json!(Utc::now().to_rfc3339());
                        doc["started_at"] =
                            json!((Utc::now() - chrono::Duration::seconds(16)).to_rfc3339());
                        serde_json::to_value(cm(name, "witness.json", doc)).unwrap()
                    }
                } else {
                    json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":status,"message":"arbitrary credentials or upstream details"})
                };
                Ok::<_, std::io::Error>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(value.to_string()))
                        .unwrap(),
                )
            }
        });
        let state = AppState::for_test_client(kube::Client::new(service, "work"), "work");
        let app = crate::routes::router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/operator/datapath-witness")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["state"],
            if status == 200 {
                "observed"
            } else {
                "unavailable"
            }
        );
        if status == 200 {
            assert_eq!(value["scope"], if all { "all" } else { "selected" });
        }
        assert!(!String::from_utf8_lossy(&bytes).contains("arbitrary credentials"));
        assert_eq!(calls.lock().unwrap().len(), 2);
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/api/operator/datapath-witness")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 405);
        }
        assert_eq!(calls.lock().unwrap().len(), 2);
    }
}
