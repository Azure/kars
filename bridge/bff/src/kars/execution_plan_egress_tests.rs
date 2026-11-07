// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

fn fetch_plan() -> ExecutionPlan {
    let mut plan = super::super::task::execution_plan::tests::filesystem_plan();
    let phase = &mut plan.roles[0].phases[0];
    phase.capabilities.push("network".into());
    phase.required_tool_calls = serde_json::from_value(json!([
        {"name":"http_fetch","arguments":{"url":"https://www.freshworks.com/freshdesk/pricing/","method":"GET"}},
        {"name":"http_fetch","arguments":{"url":"https://support.freshdesk.com/support/solutions","method":"GET"}}
    ])).unwrap();
    phase.min_tool_calls = 2;
    phase.max_tool_calls = 4;
    plan
}

#[test]
fn required_fetches_reject_unrelated_hosts_and_wrong_ports_without_mutating_the_plan() {
    let plan = fetch_plan();
    let before = plan.clone();
    for extra in [
        "freshworks.com",
        "notfreshdesk.com",
        "desk.com",
        "freshdesk.com.evil.test",
        "*.freshdesk.com",
    ] {
        let error = validate_required_egress(&plan, [("freshworks.com", None), (extra, Some(443))])
            .unwrap_err();
        assert!(error.contains("support.freshdesk.com:443"), "{error}");
        assert!(!error.contains("www.freshworks.com:443"), "{error}");
    }
    for port in [80, 0, -1, 65536] {
        assert!(
            validate_required_egress(
                &plan,
                [
                    ("freshworks.com", None),
                    ("support.freshdesk.com", Some(port))
                ]
            )
            .is_err()
        );
    }
    assert!(validate_required_egress(&plan, []).is_err());
    assert_eq!(plan, before);
}

#[test]
fn required_fetch_coverage_preserves_router_parent_domain_and_case_semantics() {
    let plan = fetch_plan();
    for domain in ["support.freshdesk.com", "freshdesk.com", "FRESHDESK.COM"] {
        validate_required_egress(&plan, [("freshworks.com", None), (domain, Some(443))]).unwrap();
    }
    validate_required_egress(
        &plan,
        [
            ("www.freshworks.com", Some(443)),
            ("support.freshdesk.com", None),
        ],
    )
    .unwrap();
    validate_required_egress(
        &super::super::task::execution_plan::tests::filesystem_plan(),
        [],
    )
    .unwrap();
}

#[test]
fn mission_and_team_activation_check_root_and_explicit_role_egress() {
    for (kind, depth) in [
        ("KarsTask", REVIEWED_TASK_DELEGATION_DEPTH),
        ("KarsTeam", REVIEWED_TEAM_DELEGATION_DEPTH),
    ] {
        let mut object = json!({"kind":kind, "metadata":{"annotations":{MARKER:"execution-plan/v1"}},
            "spec":{"blueprint":{"executionPlan":fetch_plan(),"egress":[{"host":"freshworks.com"}]},
                "envelope":{"delegationDepth":depth}}});
        let before = object.clone();
        assert!(
            validate(&object, true)
                .unwrap_err()
                .contains("support.freshdesk.com")
        );
        validate(&object, false).unwrap();
        assert_eq!(object, before);
        object["spec"]["blueprint"]["egress"] =
            json!([{"host":"freshworks.com"},{"host":"support.freshdesk.com","port":443}]);
        validate(&object, true).unwrap();
        if kind == "KarsTeam" {
            object["spec"]["roster"] = json!([{"name":"reviewer"}]);
            validate(&object, true).unwrap();
            object["spec"]["roster"][0]["blueprint"] =
                json!({"executionPlan":fetch_plan(),"egress":[{"host":"freshworks.com"}]});
            assert!(
                validate(&object, true)
                    .unwrap_err()
                    .contains("support.freshdesk.com")
            );
            object["spec"]["roster"][0]["blueprint"]["egress"] =
                object["spec"]["blueprint"]["egress"].clone();
            validate(&object, true).unwrap();
        }
    }
}
