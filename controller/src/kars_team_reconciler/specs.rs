// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure generation and validation of the authority a Team delegates.

use crate::kars_task::{KarsTaskSpec, TaskBlueprint, TaskEnvelope, TaskExecution};
use crate::kars_team::{KarsTeam, TeamRole};
use crate::mcp_server::LocalObjectRef;

/// Matches the existing KarsTask objective CEL rule; covered by a drift test.
pub(crate) const MAX_OBJECTIVE_CHARS: usize = 4096;

/// The foundation cannot enforce durable total/subtree or monetary budgets.
/// Finite budgets are valid plans, but must never become running tasks.
pub(crate) fn has_positive_budget(envelope: &TaskEnvelope) -> bool {
    envelope.budget.as_ref().is_some_and(|budget| {
        budget.tokens.is_some_and(|value| value > 0)
            || budget.usd_micros.is_some_and(|value| value > 0)
    })
}

pub(crate) fn default_member_envelope(parent: &TaskEnvelope) -> TaskEnvelope {
    let tier = parent
        .tier
        .saturating_sub(1)
        .max(1)
        .min(parent.authority_ceiling);
    TaskEnvelope {
        tier,
        authority_ceiling: tier,
        // A zero-depth parent is rejected by validation, not made delegatable.
        delegation_depth: parent.delegation_depth.saturating_sub(1).max(0),
        budget: parent.budget.clone(),
        tool_policy_ref: parent.tool_policy_ref.clone(),
        egress_allowlist_ref: parent.egress_allowlist_ref.clone(),
    }
}

/// An explicit role blueprint is a complete override. In particular, [] egress
/// is not distinguishable from an omitted Vec and must never inherit more egress.
pub(crate) fn member_blueprint(team: &KarsTeam, role: &TeamRole) -> Option<TaskBlueprint> {
    let mut blueprint = role
        .blueprint
        .clone()
        .or_else(|| team.spec.blueprint.clone());
    if let Some(member) = &mut blueprint
        && member.credential_bindings.is_none()
    {
        member.credential_bindings = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|b| b.credential_bindings.clone());
    }
    if let Some(member) = &mut blueprint
        && member.github_binding.is_none()
    {
        member.github_binding = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|b| b.github_binding.clone());
    }
    blueprint
}

pub(crate) fn principal_spec(team: &KarsTeam) -> KarsTaskSpec {
    KarsTaskSpec {
        objective: format!("[principal] {}", team.spec.charter),
        envelope: team.spec.envelope.clone(),
        parent_ref: None,
        execution: None,
        blueprint: team.spec.blueprint.clone(),
        display_name: Some(format!("{} — principal", display_name(team))),
    }
}

pub(crate) fn member_spec(team: &KarsTeam, role: &TeamRole) -> KarsTaskSpec {
    KarsTaskSpec {
        objective: role
            .system_prompt
            .clone()
            .unwrap_or_else(|| format!("[{}] {}", role.name, team.spec.charter)),
        envelope: role
            .envelope
            .clone()
            .unwrap_or_else(|| default_member_envelope(&team.spec.envelope)),
        parent_ref: Some(LocalObjectRef {
            name: principal_name(team),
        }),
        execution: None,
        blueprint: member_blueprint(team, role),
        display_name: Some(format!("{} — {}", display_name(team), role.name)),
    }
}

fn run_objective_prefix(team: &KarsTeam) -> String {
    format!(
        "Standing-operation run for team '{}'. Charter: {}",
        display_name(team),
        team.spec.charter,
    )
}

pub(crate) fn run_knowledge_budget(team: &KarsTeam) -> Result<usize, String> {
    let fixed_chars = run_objective_prefix(team).chars().count();
    MAX_OBJECTIVE_CHARS.checked_sub(fixed_chars).ok_or_else(|| {
        format!(
            "cadence objective fixed prefix (team identity and charter) is {fixed_chars} characters, exceeding the KarsTask limit of {MAX_OBJECTIVE_CHARS}; shorten the charter or team display name"
        )
    })
}

pub(crate) fn run_spec(team: &KarsTeam, knowledge: &str) -> Result<KarsTaskSpec, String> {
    let remaining = run_knowledge_budget(team)?;
    if knowledge.chars().count() > remaining {
        return Err(format!(
            "cadence history exceeds the remaining objective allowance of {remaining} characters; refusing to truncate JSON references or the charter"
        ));
    }
    let mut objective = run_objective_prefix(team);
    objective.push_str(knowledge);
    Ok(KarsTaskSpec {
        objective,
        envelope: default_member_envelope(&team.spec.envelope),
        parent_ref: Some(LocalObjectRef {
            name: principal_name(team),
        }),
        execution: Some(TaskExecution {
            launch: !team.spec.paused,
            runtime: None,
        }),
        blueprint: team.spec.blueprint.clone(),
        display_name: Some(format!("{} — standing run", display_name(team))),
    })
}

fn display_name(team: &KarsTeam) -> String {
    team.spec
        .display_name
        .clone()
        .or_else(|| team.metadata.name.clone())
        .unwrap_or_default()
}

pub(crate) fn principal_name(team: &KarsTeam) -> String {
    format!(
        "{}-principal",
        team.metadata.name.as_deref().unwrap_or_default()
    )
}

pub(crate) fn member_name(team: &KarsTeam, role: &TeamRole) -> String {
    format!(
        "{}-{}",
        team.metadata.name.as_deref().unwrap_or_default(),
        sanitize(&role.name)
    )
}

pub(crate) fn sanitize(s: &str) -> String {
    let value: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let value = value.trim_matches('-');
    if value.is_empty() {
        "role".into()
    } else {
        value.into()
    }
}

pub(crate) fn envelope_errors(env: &TaskEnvelope) -> Vec<String> {
    let mut errors = Vec::new();
    if !(1..=5).contains(&env.tier) {
        errors.push("envelope.tier must be in 1..5".into());
    }
    if !(1..=5).contains(&env.authority_ceiling) || env.authority_ceiling > env.tier {
        errors.push("envelope.authorityCeiling must be in 1..5 and <= tier".into());
    }
    if env.delegation_depth < 0 {
        errors.push("envelope.delegationDepth must be >= 0".into());
    }
    if let Some(budget) = &env.budget {
        if budget.tokens.is_some_and(|n| n < 0) {
            errors.push("envelope.budget.tokens must be >= 0".into());
        }
        if budget.usd_micros.is_some_and(|n| n < 0) {
            errors.push("envelope.budget.usdMicros must be >= 0".into());
        }
    }
    errors
}

pub(crate) fn policy_errors(spec: &KarsTaskSpec) -> Vec<String> {
    let envelope = spec
        .envelope
        .tool_policy_ref
        .as_ref()
        .map(|r| r.name.as_str());
    let blueprint = spec
        .blueprint
        .as_ref()
        .and_then(|b| b.tool_policy.as_deref());
    if spec
        .blueprint
        .as_ref()
        .is_some_and(|blueprint| !blueprint.mcp_servers.is_empty())
        && crate::kars_task::effective_tool_policy(spec).is_none()
    {
        vec!["MCP servers require a bounding tool policy".into()]
    } else if envelope.is_some_and(|p| p.trim().is_empty())
        || blueprint.is_some_and(|p| p.trim().is_empty())
        || matches!((envelope, blueprint), (Some(a), Some(b)) if a != b)
    {
        vec!["blueprint and envelope tool policies must name the same nonempty bound".into()]
    } else {
        Vec::new()
    }
}
