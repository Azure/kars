// kars Bridge BFF — launch qualification regression tests.

use super::{
    QualifiedResourceSelection, channel_resource_selection, mcp_resource_selection,
    resource_is_qualified_in, resource_qualification_routes_in, route_is_qualified_in,
    route_qualification_gap_in,
};
use std::collections::BTreeSet;

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
