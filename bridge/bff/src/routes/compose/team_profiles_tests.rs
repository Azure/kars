// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::routes::compose::capability_tests::{test_efficiency, test_options};
use crate::routes::compose::team_proposal::parse_and_validate_team;
use crate::routes::operator::ProfileRoleDto;
use crate::routes::options::RefOption;
use serde_json::json;

fn reference(name: &str) -> RefOption {
    RefOption {
        name: name.into(),
        namespace: "workspace".into(),
        summary: None,
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: None,
        compiled_digest: None,
        backend: None,
        readiness: None,
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: Vec::new(),
    }
}

fn fixture() -> (Options, Vec<ProfileDto>, serde_json::Value) {
    let mut options = test_options();
    options.skills.push(reference("review"));
    options.tool_policies.push(reference("restricted"));
    options.mcp_servers.push(reference("browser"));
    let profiles = vec![ProfileDto {
        name: "adoption".into(),
        namespace: "workspace".into(),
        domain: Some("software".into()),
        phase: Some("Ready".into()),
        template_digest: Some("template-v1".into()),
        display_name: Some("Software adoption".into()),
        charter_template: Some("Review public software".into()),
        tier: Some(3),
        tool_policy: Some("restricted".into()),
        knowledge_commons: Some("reviews".into()),
        roles: vec![ProfileRoleDto {
            name: "reviewer".into(),
            system_prompt: Some("Inspect independent evidence.".into()),
            skills: vec!["review".into()],
        }],
        spec: json!({"defaultEnvelope": {"tier":3,"authorityCeiling":2,"delegationDepth":0,
            "budget":{"scope":"GovernedInference","tokens":2000000,"usdMicros":500000},
            "toolPolicyRef":{"name":"restricted"}}}),
    }];
    let raw = json!({"profile":"adoption","tier":3,"budget_tokens":1800000,
        "instructions":"Assess the requested software", "mcp_servers":["browser"],
        "egress":[{"host":"example.com","port":443}], "egress_mode":"strict",
        "roles":[{"name":"reviewer","system_prompt":"Inspect the requested website.","skills":["review"]}]});
    (options, profiles, raw)
}

#[test]
fn reviewed_spending_inherits_selected_and_proposed_live_profiles() {
    for selected in [None, Some("adoption")] {
        for requested in [None, Some(1000000), Some(3000000)] {
            let (options, profiles, raw) = fixture();
            let (mut proposal, _, _) =
                parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
            let spending = super::super::spending::RequestedSpending {
                budget_tokens: requested,
                ..Default::default()
            };
            apply_reviewed_profile(
                &raw.to_string(),
                &mut proposal,
                &profiles,
                &options,
                selected,
                &spending,
            )
            .unwrap();
            assert_eq!(
                proposal.budget_tokens,
                Some(requested.unwrap_or(2000000).min(2000000))
            );
            assert_eq!(proposal.profile.unwrap().name, "adoption");
        }
    }
    let (options, mut profiles, raw) = fixture();
    profiles[0].spec["defaultEnvelope"]["budget"] = json!({"scope":"GovernedInference"});
    let (mut proposal, _, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    apply_reviewed_profile(
        &raw.to_string(),
        &mut proposal,
        &profiles,
        &options,
        None,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(proposal.budget_tokens, None);
    profiles[0].phase = Some("Pending".into());
    assert!(
        apply_reviewed_profile(
            &raw.to_string(),
            &mut proposal,
            &profiles,
            &options,
            None,
            &Default::default()
        )
        .is_err()
    );
    assert!(
        apply_reviewed_profile(
            &raw.to_string(),
            &mut proposal,
            &[],
            &options,
            None,
            &Default::default()
        )
        .is_err()
    );
}

#[test]
fn team_proposals_default_to_strict_and_reject_unsupported_egress() {
    let (options, _, raw) = fixture();
    for mode in [json!(null), json!("strict"), json!(" Strict ")] {
        let mut raw = raw.clone();
        raw["egress_mode"] = mode;
        let (proposal, _, error) =
            parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
        assert_eq!(proposal.egress_mode, "strict");
        assert!(error.is_none(), "{error:?}");
        assert_eq!(proposal.egress[0].host, "example.com");
    }
    let mut missing = raw.clone();
    missing.as_object_mut().unwrap().remove("egress_mode");
    let (proposal, _, error) =
        parse_and_validate_team(&missing.to_string(), &options, &test_efficiency());
    assert_eq!(proposal.egress_mode, "strict");
    assert!(error.is_none(), "{error:?}");
    for mode in [json!("learning"), json!("Learn"), json!(""), json!(false)] {
        let mut raw = raw.clone();
        raw["egress_mode"] = mode;
        let (proposal, _, error) =
            parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
        assert_eq!(proposal.egress_mode, "strict");
        assert!(error.unwrap().contains("Standing Teams use Strict egress"));
        assert_eq!(proposal.egress[0].host, "example.com");
    }
}

#[test]
fn catalogue_profile_binds_defaults_without_inventing_qualification() {
    let (options, profiles, raw) = fixture();
    let prompt = profile_prompt(&profiles, &options, None);
    assert!(prompt.contains("adoption"));
    assert!(prompt.contains("template-v1"));
    assert!(prompt.contains("Ready means template validation only"));
    let (mut proposal, _, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    apply_profile(&raw.to_string(), &mut proposal, &profiles, &options, None).unwrap();
    let profile = proposal.profile.as_ref().unwrap();
    assert_eq!(profile.spec["defaultEnvelope"]["delegationDepth"], 0);
    assert_eq!(
        profile.spec["defaultEnvelope"]["budget"]["usdMicros"],
        500000
    );
    assert_eq!(
        profile.spec["defaultEnvelope"]["toolPolicyRef"]["name"],
        "restricted"
    );
    assert_eq!(proposal.budget_tokens, Some(1800000));
    assert_eq!(proposal.roles[0].skills, vec!["review"]);
    assert!(
        proposal.roles[0]
            .system_prompt
            .contains("Inspect independent evidence.")
    );
    assert!(
        proposal.roles[0]
            .system_prompt
            .contains("Inspect the requested website.")
    );
    assert_eq!(proposal.mcp_servers, vec!["browser"]);
    assert_eq!(proposal.egress[0].host, "example.com");
    assert!(options.mcp_servers[0].qualified_routes.is_empty());
}

#[test]
fn scope_only_and_currency_only_profiles_preserve_the_entire_catalogue_authority() {
    let (options, profiles, raw) = fixture();
    for budget in [
        json!({"scope":"GovernedInference"}),
        json!({"scope":"GovernedInference","tokens":null,"usdMicros":null}),
        json!({"scope":"GovernedInference","usdMicros":500000}),
    ] {
        for tokens in [json!(null), json!(1800000)] {
            let mut profiles = profiles.clone();
            profiles[0].spec["defaultEnvelope"]["budget"] = budget.clone();
            let mut raw = raw.clone();
            raw["budget_tokens"] = tokens.clone();
            let (mut proposal, _, _) =
                parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
            let before_resources = serde_json::to_value(&proposal.roles[0].resources).unwrap();
            apply_profile(
                &raw.to_string(),
                &mut proposal,
                &profiles,
                &options,
                Some("adoption"),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(proposal.profile.as_ref().unwrap()).unwrap(),
                serde_json::to_value(&profiles[0]).unwrap()
            );
            assert_eq!(proposal.budget_tokens, tokens.as_i64());
            assert_eq!(
                serde_json::to_value(&proposal.roles[0].resources).unwrap(),
                before_resources
            );
            assert_eq!(proposal.roles[0].skills, vec!["review"]);
            assert!(
                proposal.roles[0]
                    .system_prompt
                    .contains("Inspect independent evidence.")
            );
            assert_eq!(proposal.mcp_servers, vec!["browser"]);
            assert_eq!(proposal.egress[0].host, "example.com");
        }
    }
}

#[test]
fn profile_selection_never_drops_bounds_roles_or_skills_to_repair_a_package() {
    let (options, profiles, raw) = fixture();
    for (key, value, expected) in [
        ("profile", json!("unknown"), "not installed"),
        ("profile", json!({"name":"adoption"}), "name or null"),
        ("tier", json!(5), "default tier"),
        ("budget_tokens", json!(2000001), "at most"),
        ("budget_tokens", json!(null), "at most"),
        ("budget_tokens", json!(0), "positive safe"),
        ("budget_tokens", json!(-1), "positive safe"),
        (
            "budget_tokens",
            json!(9007199254740992_i64),
            "positive safe",
        ),
        ("budget_tokens", json!(1.5), "positive safe"),
        ("budget_tokens", json!("unlimited"), "positive safe"),
        ("roles", json!([]), "requires role"),
        (
            "roles",
            json!([{"name":"reviewer","skills":[]}]),
            "requires skill",
        ),
    ] {
        let mut rejected = raw.clone();
        rejected[key] = value;
        let (mut proposal, _, _) =
            parse_and_validate_team(&rejected.to_string(), &options, &test_efficiency());
        assert!(
            apply_profile(
                &rejected.to_string(),
                &mut proposal,
                &profiles,
                &options,
                None
            )
            .unwrap_err()
            .contains(expected)
        );
        assert!(proposal.profile.is_none());
    }
    let mut omitted = raw.clone();
    omitted["profile"] = json!(null);
    let (mut proposal, _, _) =
        parse_and_validate_team(&omitted.to_string(), &options, &test_efficiency());
    assert!(
        apply_profile(
            &omitted.to_string(),
            &mut proposal,
            &profiles,
            &options,
            Some("adoption")
        )
        .is_err()
    );
    apply_profile(
        &omitted.to_string(),
        &mut proposal,
        &profiles,
        &options,
        None,
    )
    .unwrap();
    assert!(proposal.profile.is_none());
    let (mut proposal, _, _) =
        parse_and_validate_team(&raw.to_string(), &options, &test_efficiency());
    assert!(apply_profile(&raw.to_string(), &mut proposal, &[], &options, None).is_err());
}

#[test]
fn nonready_unsupported_or_unavailable_profile_settings_are_truthful_blockers() {
    let (mut options, profiles, _) = fixture();
    for patch in [
        json!({"extraPermission":true}),
        json!({"egressAllowlistRef":{"name":"broad"}}),
        json!({"tier":0}),
        json!({"authorityCeiling":4}),
        json!({"delegationDepth":-1}),
        json!({"budget":{"scope":"Planning","tokens":10}}),
        json!({"budget":{"scope":"GovernedInference","tokens":9007199254740992_i64}}),
        json!({"budget":{"scope":"GovernedInference","tokens":0}}),
        json!({"budget":{"scope":"GovernedInference","tokens":-1}}),
        json!({"budget":{"scope":"GovernedInference","tokens":1.5}}),
        json!({"budget":{"scope":"GovernedInference","tokens":"unlimited"}}),
        json!({"budget":{"scope":"GovernedInference","tokens":true}}),
        json!({"budget":{"scope":"GovernedInference","usdMicros":-1}}),
        json!({"budget":{"scope":"GovernedInference","usdMicros":9007199254740992_i64}}),
        json!({"budget":{"scope":"GovernedInference","usdMicros":"unlimited"}}),
        json!({"budget":{"scope":"GovernedInference","tokens":10,"usdMicros":0}}),
        json!({"toolPolicyRef":{"name":"restricted","namespace":"elsewhere"}}),
    ] {
        let mut profile = profiles[0].clone();
        profile.spec["defaultEnvelope"]
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(envelope(&profile, &options).is_err());
    }
    for phase in [None, Some("Pending"), Some("Degraded")] {
        let mut profile = profiles[0].clone();
        profile.phase = phase.map(str::to_string);
        assert!(
            envelope(&profile, &options)
                .unwrap_err()
                .contains("not Ready")
        );
    }
    options.skills.clear();
    assert!(
        envelope(&profiles[0], &options)
            .unwrap_err()
            .contains("skill")
    );
    assert!(
        profile_prompt(&profiles, &options, Some("adoption"))
            .contains("cannot be silently removed")
    );
    options.tool_policies.clear();
    assert!(
        envelope(&profiles[0], &options)
            .unwrap_err()
            .contains("policy")
    );
}
