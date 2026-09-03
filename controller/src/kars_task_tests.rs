// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn sample_envelope() -> TaskEnvelope {
    TaskEnvelope {
        tier: 3,
        budget: Some(TaskBudget {
            tokens: Some(100_000),
            usd_micros: Some(5_000_000),
        }),
        tool_policy_ref: Some(LocalObjectRef {
            name: "default-tools".into(),
        }),
        egress_allowlist_ref: None,
        delegation_depth: 2,
        authority_ceiling: 3,
    }
}

#[test]
fn envelope_digest_is_deterministic() {
    let envelope = sample_envelope();
    assert_eq!(envelope.digest(), envelope.digest());
}

#[test]
fn envelope_digest_has_sha256_prefix_and_length() {
    let digest = sample_envelope().digest();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(digest.len(), 39);
}

#[test]
fn envelope_digest_changes_with_tier() {
    let mut envelope = sample_envelope();
    let before = envelope.digest();
    envelope.tier = 4;
    assert_ne!(before, envelope.digest());
}

#[test]
fn envelope_digest_changes_with_delegation_depth() {
    let mut envelope = sample_envelope();
    let before = envelope.digest();
    envelope.delegation_depth += 1;
    assert_ne!(before, envelope.digest());
}

#[test]
fn spec_roundtrips_through_camelcase_yaml() {
    let spec = KarsTaskSpec {
        objective: "fix the flaky test in payments".into(),
        envelope: sample_envelope(),
        parent_ref: None,
        execution: None,
        blueprint: None,
        display_name: Some("payments-bugfix".into()),
    };
    let yaml = serde_yaml::to_string(&spec).expect("serializes");
    assert!(yaml.contains("authorityCeiling:"));
    assert!(yaml.contains("delegationDepth:"));
    let back: KarsTaskSpec = serde_yaml::from_str(&yaml).expect("roundtrips");
    assert_eq!(back.envelope.tier, 3);
    assert_eq!(back.envelope.authority_ceiling, 3);
}

fn parent_envelope() -> TaskEnvelope {
    TaskEnvelope {
        tier: 5,
        budget: Some(TaskBudget {
            tokens: Some(1_000_000),
            usd_micros: Some(50_000_000),
        }),
        tool_policy_ref: Some(LocalObjectRef {
            name: "strict-tools".into(),
        }),
        egress_allowlist_ref: None,
        delegation_depth: 3,
        authority_ceiling: 4,
    }
}

#[test]
fn valid_child_attenuates_on_every_axis() {
    let parent = parent_envelope();
    let child = TaskEnvelope {
        tier: 4,
        budget: Some(TaskBudget {
            tokens: Some(100_000),
            usd_micros: Some(5_000_000),
        }),
        tool_policy_ref: Some(LocalObjectRef {
            name: "strict-tools".into(),
        }),
        egress_allowlist_ref: None,
        delegation_depth: 2,
        authority_ceiling: 3,
    };
    assert!(child.attenuation_violations(&parent).is_empty());
}

#[test]
fn child_tier_above_parent_ceiling_is_amplification() {
    let parent = parent_envelope();
    let child = TaskEnvelope {
        tier: 5,
        authority_ceiling: 4,
        delegation_depth: 0,
        ..parent_envelope()
    };
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(
                    violation,
                    EnvelopeViolation::TierExceedsParentCeiling { .. }
                )
            })
    );
}

#[test]
fn child_ceiling_above_parent_ceiling_is_amplification() {
    let parent = parent_envelope();
    let mut child = parent_envelope();
    child.tier = 4;
    child.authority_ceiling = 5;
    child.delegation_depth = 0;
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(
                    violation,
                    EnvelopeViolation::CeilingExceedsParentCeiling { .. }
                )
            })
    );
}

#[test]
fn delegation_depth_must_decrement() {
    let parent = parent_envelope();
    let mut child = parent_envelope();
    child.tier = 4;
    child.authority_ceiling = 4;
    child.delegation_depth = 3;
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(violation, EnvelopeViolation::DelegationDepthExceeded { .. })
            })
    );
}

#[test]
fn exhausted_delegation_budget_rejects_any_child() {
    let mut parent = parent_envelope();
    parent.delegation_depth = 0;
    let mut child = parent_envelope();
    child.tier = 1;
    child.authority_ceiling = 1;
    child.delegation_depth = 0;
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(violation, EnvelopeViolation::DelegationDepthExceeded { .. })
            })
    );
}

#[test]
fn child_budget_over_parent_cap_is_amplification() {
    let parent = parent_envelope();
    let mut child = parent_envelope();
    child.tier = 4;
    child.authority_ceiling = 3;
    child.delegation_depth = 1;
    child.budget = Some(TaskBudget {
        tokens: Some(2_000_000),
        usd_micros: Some(1_000_000),
    });
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(
                    violation,
                    EnvelopeViolation::BudgetExceeded {
                        axis: BudgetAxis::Tokens,
                        ..
                    }
                )
            })
    );
}

#[test]
fn unbounded_child_under_bounded_parent_is_amplification() {
    let parent = parent_envelope();
    let mut child = parent_envelope();
    child.tier = 4;
    child.authority_ceiling = 3;
    child.delegation_depth = 1;
    child.budget = None;
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| matches!(violation, EnvelopeViolation::BudgetUnbounded { .. }))
    );
}

#[test]
fn child_must_match_parent_pinned_tool_policy() {
    let parent = parent_envelope();
    let mut child = parent_envelope();
    child.tier = 4;
    child.authority_ceiling = 3;
    child.delegation_depth = 1;
    child.tool_policy_ref = Some(LocalObjectRef {
        name: "looser-tools".into(),
    });
    assert!(
        child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(
                    violation,
                    EnvelopeViolation::PolicyMismatch {
                        axis: PolicyAxis::ToolPolicy,
                        ..
                    }
                )
            })
    );
}

#[test]
fn child_may_add_egress_bound_where_parent_has_none() {
    let parent = parent_envelope();
    let mut child = parent_envelope();
    child.tier = 4;
    child.authority_ceiling = 3;
    child.delegation_depth = 1;
    child.egress_allowlist_ref = Some(LocalObjectRef {
        name: "tighter-egress".into(),
    });
    assert!(
        !child
            .attenuation_violations(&parent)
            .iter()
            .any(|violation| {
                matches!(
                    violation,
                    EnvelopeViolation::PolicyMismatch {
                        axis: PolicyAxis::EgressAllowlist,
                        ..
                    }
                )
            })
    );
}

#[test]
fn default_envelope_is_least_privilege() {
    let envelope = TaskEnvelope::default();
    assert_eq!(envelope.tier, TIER_MIN);
    assert_eq!(envelope.delegation_depth, 0);
    assert_eq!(envelope.authority_ceiling, TIER_MIN);
    assert!(envelope.budget.is_none());
}

fn spec_with(
    envelope: TaskEnvelope,
    tool_policy: Option<&str>,
    egress: Vec<TaskEgress>,
) -> KarsTaskSpec {
    KarsTaskSpec {
        objective: "x".into(),
        envelope,
        parent_ref: None,
        execution: None,
        blueprint: Some(TaskBlueprint {
            tool_policy: tool_policy.map(str::to_string),
            egress,
            ..Default::default()
        }),
        display_name: None,
    }
}

fn egress(host: &str, port: Option<u16>) -> TaskEgress {
    TaskEgress {
        host: host.into(),
        port,
    }
}

fn child_envelope() -> TaskEnvelope {
    TaskEnvelope {
        tier: 4,
        budget: Some(TaskBudget {
            tokens: Some(100_000),
            usd_micros: Some(5_000_000),
        }),
        tool_policy_ref: Some(LocalObjectRef {
            name: "strict-tools".into(),
        }),
        egress_allowlist_ref: None,
        delegation_depth: 2,
        authority_ceiling: 4,
    }
}

#[test]
fn effective_tool_policy_prefers_blueprint_then_envelope() {
    let spec = spec_with(parent_envelope(), Some("bp-tools"), vec![]);
    assert_eq!(effective_tool_policy(&spec), Some("bp-tools"));
    let fallback = spec_with(parent_envelope(), None, vec![]);
    assert_eq!(effective_tool_policy(&fallback), Some("strict-tools"));
}

#[test]
fn child_egress_must_be_subset_of_parent() {
    let parent = spec_with(
        parent_envelope(),
        Some("strict-tools"),
        vec![
            egress("api.github.com", Some(443)),
            egress("pkg.go.dev", None),
        ],
    );
    let valid = spec_with(
        child_envelope(),
        Some("strict-tools"),
        vec![
            egress("api.github.com", Some(443)),
            egress("pkg.go.dev", Some(443)),
        ],
    );
    assert!(spec_attenuation_violations(&valid, &parent).is_empty());
    let invalid = spec_with(
        child_envelope(),
        Some("strict-tools"),
        vec![egress("evil.example.com", Some(443))],
    );
    let violations = spec_attenuation_violations(&invalid, &parent);
    assert!(matches!(
        violations.as_slice(),
        [EnvelopeViolation::EgressNotSubset { host, .. }] if host == "evil.example.com"
    ));
}

#[test]
fn empty_parent_egress_permits_no_child_egress() {
    let parent = spec_with(parent_envelope(), Some("strict-tools"), vec![]);
    let child = spec_with(
        child_envelope(),
        Some("strict-tools"),
        vec![egress("api.github.com", Some(443))],
    );
    assert!(
        spec_attenuation_violations(&child, &parent)
            .iter()
            .any(|violation| matches!(violation, EnvelopeViolation::EgressNotSubset { .. }))
    );
}

#[test]
fn child_tool_policy_must_match_parent_effective() {
    let parent = spec_with(parent_envelope(), Some("strict-tools"), vec![]);
    let child = spec_with(child_envelope(), Some("loose-tools"), vec![]);
    assert!(
        spec_attenuation_violations(&child, &parent)
            .iter()
            .any(|violation| {
                matches!(
                    violation,
                    EnvelopeViolation::PolicyMismatch {
                        axis: PolicyAxis::ToolPolicy,
                        ..
                    }
                )
            })
    );
}
