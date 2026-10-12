// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — launch qualification regression tests.

use super::{
    QualifiedResourceSelection, channel_resource_selection, mcp_resource_selection,
    resource_is_qualified_in, resource_qualification_routes_in, route_is_qualified_in,
    route_qualification_gap_in,
};
use std::collections::BTreeSet;

#[test]
fn public_resource_admission_never_reports_missing_evidence_as_qualified() {
    const CHILD: &str = "KARS_RESOURCE_ADMISSION_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for mode in ["required", "validation"] {
            for records in ["[]", "{"] {
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "routes::options::qualification_tests::public_resource_admission_never_reports_missing_evidence_as_qualified"])
                    .env(CHILD, "1")
                    .env("BRIDGE_ROUTE_QUALIFICATION_MODE", mode)
                    .env("BRIDGE_QUALIFICATION_RECORDS_JSON", records)
                    .env_remove("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
                    .output().unwrap();
                assert!(
                    result.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
            }
        }
        return;
    }
    let option = super::RefOption {
        name: "approved-resource".into(),
        namespace: "demo".into(),
        summary: None,
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: Some("sha256:tools-v1".into()),
        compiled_digest: Some("sha256:memory-v1".into()),
        backend: Some("foundry".into()),
        readiness: Some("Ready".into()),
        version: Some("1.0.0".into()),
        recipe: None,
        version_digest: Some("sha256:skill-v1".into()),
        qualified_routes: Vec::new(),
    };
    type Check = fn(&str, &str, &str, &super::RefOption) -> Result<bool, String>;
    let checks: [(Check, Check); 3] = [
        (
            super::mcp_server_qualified_for_route,
            super::mcp_server_admitted_for_route,
        ),
        (
            super::memory_binding_qualified_for_route,
            super::memory_binding_admitted_for_route,
        ),
        (
            super::skill_version_qualified_for_route,
            super::skill_version_admitted_for_route,
        ),
    ];
    for (qualified, admitted) in checks {
        let proof = qualified("OpenClaw", "foundry", "model", &option);
        let admission = admitted("OpenClaw", "foundry", "model", &option);
        if std::env::var("BRIDGE_QUALIFICATION_RECORDS_JSON").unwrap() == "{" {
            assert!(proof.is_err());
            assert!(admission.is_err());
        } else {
            assert!(!proof.unwrap());
            assert_eq!(
                admission.unwrap(),
                std::env::var("BRIDGE_ROUTE_QUALIFICATION_MODE").unwrap() == "validation"
            );
        }
    }
}

#[test]
fn resource_validation_admission_preserves_bindings_and_never_creates_evidence() {
    use super::RouteQualificationMode as Mode;
    use super::qualification::resource_admitted_in;
    for (kind, backend, schema, version) in [
        ("skill", None, None, Some("sha256:skill-v1")),
        ("mcp", None, Some("sha256:tools-v1"), None),
        ("memory", Some("foundry"), Some("sha256:binding-v1"), None),
    ] {
        let resource = QualifiedResourceSelection {
            kind: kind.into(),
            name: "installed-resource".into(),
            backend: backend.map(str::to_string),
            schema_digest: schema.map(str::to_string),
            version_digest: version.map(str::to_string),
        };
        for mode in [Mode::Required, Mode::Validation] {
            assert_eq!(
                resource_admitted_in(mode, "[]", "OpenClaw", "foundry", "model", &resource)
                    .unwrap(),
                mode == Mode::Validation,
            );
            assert!(
                resource_admitted_in(mode, "{", "OpenClaw", "foundry", "model", &resource).is_err()
            );
        }
        assert!(
            !resource_is_qualified_in("[]", "OpenClaw", "foundry", "model", &resource).unwrap()
        );
        assert!(
            resource_qualification_routes_in("[]", &resource)
                .unwrap()
                .is_empty()
        );
        for missing in [None, Some(" \t".into())] {
            let mut incomplete = resource.clone();
            if kind == "skill" {
                incomplete.version_digest = missing.clone();
            } else {
                incomplete.schema_digest = missing.clone();
            }
            assert!(
                !resource_admitted_in(
                    Mode::Validation,
                    "[]",
                    "OpenClaw",
                    "foundry",
                    "model",
                    &incomplete
                )
                .unwrap()
            );
            if kind == "memory" {
                let mut incomplete = resource.clone();
                incomplete.backend = missing;
                assert!(
                    !resource_admitted_in(
                        Mode::Validation,
                        "[]",
                        "OpenClaw",
                        "foundry",
                        "model",
                        &incomplete
                    )
                    .unwrap()
                );
            }
        }
        for kind in ["channel", "unknown"] {
            let mut unsupported = resource.clone();
            unsupported.kind = kind.into();
            assert!(
                !resource_admitted_in(
                    Mode::Validation,
                    "[]",
                    "OpenClaw",
                    "foundry",
                    "model",
                    &unsupported
                )
                .unwrap()
            );
        }
        let mut unnamed = resource;
        unnamed.name = " ".into();
        assert!(
            !resource_admitted_in(
                Mode::Validation,
                "[]",
                "OpenClaw",
                "foundry",
                "model",
                &unnamed
            )
            .unwrap()
        );
    }
}

#[test]
fn route_validation_mode_is_explicit_and_defaults_to_required() {
    use super::RouteQualificationMode as Mode;
    assert_eq!(Mode::parse(None).unwrap(), Mode::Required);
    assert_eq!(Mode::parse(Some("required")).unwrap(), Mode::Required);
    assert_eq!(Mode::parse(Some("validation")).unwrap(), Mode::Validation);
    for value in ["", " ", "Validation", "validation ", "disabled", "allow"] {
        assert!(Mode::parse(Some(value)).is_err(), "accepted {value:?}");
    }
    assert!(!Mode::Required.admit(Ok(false)).unwrap());
    assert!(Mode::Validation.admit(Ok(false)).unwrap());
    for mode in [Mode::Required, Mode::Validation] {
        assert!(mode.admit(Ok(true)).unwrap());
        assert_eq!(
            mode.admit(Err("bad configuration".into())).unwrap_err(),
            "bad configuration"
        );
    }
}

#[test]
fn validation_record_loading_never_invents_route_or_resource_evidence() {
    use super::{RouteQualificationMode as Mode, configured_records};
    assert!(configured_records(Mode::Required, None, None).is_err());
    let records = configured_records(Mode::Validation, None, None).unwrap();
    assert!(records.is_empty());
    let raw = serde_json::to_string(&records).unwrap();
    let capabilities = ["single-agent", "telemetry"]
        .map(String::from)
        .into_iter()
        .collect();
    assert!(
        !route_is_qualified_in(
            &raw,
            "OpenClaw",
            "foundry",
            "gpt-4.1",
            &capabilities,
            1,
            None
        )
        .unwrap()
    );
    let resource = channel_resource_selection("slack");
    assert!(!resource_is_qualified_in(&raw, "OpenClaw", "foundry", "gpt-4.1", &resource).unwrap());
    for mode in [Mode::Required, Mode::Validation] {
        assert!(
            configured_records(mode, Some("[]"), Some(" "))
                .unwrap()
                .is_empty()
        );
        for invalid in ["", "not-json", "{}", "null", "[{\"runtime\":\"OpenClaw\"}]"] {
            assert!(configured_records(mode, Some(invalid), None).is_err());
            let error = configured_records(mode, Some("[]"), Some(invalid));
            if !invalid.is_empty() {
                assert!(
                    error
                        .unwrap_err()
                        .contains("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
                );
            }
        }
    }
}

#[test]
fn qualification_requires_route_capabilities_constraints_and_evidence() {
    let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["delegation","filesystem-read","shell","network","artifacts","telemetry"],
            "max_parallel":1,
            "min_total_tokens":128144,
            "evidence":{
              "task":"openclaw-proof",
              "run_id":"run-1",
              "digest":"sha256:abc"
            }

          }
        ]"#;
    let required = ["delegation", "shell", "telemetry"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert!(
        route_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &required,
            1,
            Some(128_144)
        )
        .expect("valid routes")
    );
    assert!(
        route_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &required,
            1,
            None
        )
        .expect("an uncapped route is not below the retained minimum")
    );
    let unsupported = ["delegation", "memory"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert!(
        !route_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &unsupported,
            1,
            Some(128_144)
        )
        .expect("valid routes")
    );
    assert!(
        !route_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &required,
            2,
            Some(128_144)
        )
        .expect("valid routes")
    );
    assert!(
        !route_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &required,
            1,
            Some(100_000)
        )
        .expect("valid routes")
    );
    assert!(route_is_qualified_in("{", "OpenClaw", "x", "y", &required, 1, None).is_err());
}

#[test]
fn qualification_gap_reports_only_capabilities_missing_from_closest_record() {
    let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["delegation","web-search","network","artifacts","telemetry"],
            "max_parallel":1,
            "min_total_tokens":300000,
            "evidence":{"task":"research-proof","run_id":"run-1","digest":"sha256:abc"}
          }
        ]"#;
    let required = [
        "artifacts",
        "delegation",
        "mcp",
        "network",
        "telemetry",
        "web-search",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        route_qualification_gap_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &required,
            1,
            Some(300000),
        )
        .expect("gap"),
        ["mcp".to_string()].into_iter().collect()
    );
}

#[test]
fn resource_qualification_requires_matching_current_digest_and_ignores_generic_routes() {
    let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["mcp","network","telemetry"],
            "max_parallel":1,
            "evidence":{"task":"generic-proof","run_id":"run-1","digest":"sha256:generic"}
          },
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["mcp","network","telemetry"],
            "max_parallel":1,
            "resource":{"kind":"mcp","name":"playwright","schema_digest":"sha256:tools-v1"},
            "evidence":{"task":"mcp-proof","run_id":"run-2","digest":"sha256:mcp"}
          }
        ]"#;
    let selection = QualifiedResourceSelection {
        kind: "mcp".into(),
        name: "playwright".into(),
        backend: None,
        schema_digest: Some("sha256:tools-v1".into()),
        version_digest: None,
    };
    assert!(
        resource_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &selection,
        )
        .expect("resource qualification")
    );
    let mismatched = QualifiedResourceSelection {
        schema_digest: Some("sha256:tools-v2".into()),
        ..selection.clone()
    };
    assert!(
        !resource_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &mismatched,
        )
        .expect("resource qualification")
    );
    let missing_digest = QualifiedResourceSelection {
        schema_digest: None,
        ..selection
    };
    assert!(
        !resource_is_qualified_in(
            routes,
            "OpenClaw",
            "local-inference",
            "gpt-oss-120b",
            &missing_digest,
        )
        .expect("resource qualification")
    );
}

#[test]
fn resource_route_summary_lists_only_matching_resource_records() {
    let routes = r#"[
          {
            "runtime":"OpenClaw",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["skill","telemetry"],
            "max_parallel":1,
            "resource":{"kind":"channel","name":"telegram"},
            "evidence":{"task":"channel-proof","run_id":"run-1","digest":"sha256:chan"}
          },
          {
            "runtime":"Hermes",
            "provider":"local-inference",
            "deployment":"gpt-oss-120b",
            "capabilities":["mcp","telemetry"],
            "max_parallel":1,
            "resource":{"kind":"mcp","name":"playwright","schema_digest":"sha256:tools-v1"},
            "evidence":{"task":"mcp-proof","run_id":"run-2","digest":"sha256:mcp"}
          }
        ]"#;
    let selection = channel_resource_selection("telegram");
    assert_eq!(
        resource_qualification_routes_in(routes, &selection).expect("summary"),
        vec!["OpenClaw · local-inference::gpt-oss-120b".to_string()]
    );
    let mcp = mcp_resource_selection(&super::RefOption {
        name: "playwright".into(),
        namespace: "demo".into(),
        summary: None,
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: Some("sha256:tools-v1".into()),
        compiled_digest: None,
        backend: None,
        readiness: None,
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: Vec::new(),
    });
    assert_eq!(
        resource_qualification_routes_in(routes, &mcp).expect("summary"),
        vec!["Hermes · local-inference::gpt-oss-120b".to_string()]
    );
}
