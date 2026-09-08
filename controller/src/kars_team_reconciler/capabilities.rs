// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resolve required capabilities without launching with partial authority.

use super::{ReconcileError, specs};
use crate::kars_profile::KarsProfile;
use crate::kars_skill::KarsSkill;
use crate::kars_team::{KarsTeam, TeamRole};
use crate::mcp_server::McpServer;
use crate::status::phase::PHASE_READY;
use kube::{Api, Client};

pub(super) async fn effective_team(
    client: &Client,
    team: &KarsTeam,
) -> Result<KarsTeam, ReconcileError> {
    let ns = super::namespace(team)?;
    let mut effective = team.clone();
    if let Some(reference) = &team.spec.profile_ref {
        let profile = Api::<KarsProfile>::namespaced(client.clone(), ns)
            .get_opt(&reference.name)
            .await?
            .ok_or_else(|| {
                ReconcileError::Invalid(format!("profile '{}' is missing", reference.name))
            })?;
        let ready = profile.metadata.deletion_timestamp.is_none()
            && profile.metadata.generation.is_some()
            && profile.status.as_ref().is_some_and(|status| {
                status.phase.as_deref() == Some(PHASE_READY)
                    && status.observed_generation == profile.metadata.generation
                    && status.template_digest.as_deref() == Some(profile.template_digest().as_str())
            });
        if !ready || !profile.validation_errors().is_empty() {
            return Err(ReconcileError::Invalid(format!(
                "profile '{}' is not currently Ready",
                reference.name
            )));
        }
        if effective.spec.charter.trim().is_empty() {
            effective.spec.charter = profile.spec.charter_template.clone();
        }
        if effective.spec.roster.is_empty() {
            effective.spec.roster = profile
                .spec
                .roles
                .iter()
                .map(|role| TeamRole {
                    name: role.name.clone(),
                    system_prompt: role.system_prompt.clone(),
                    skills: role.skills.clone(),
                    ..Default::default()
                })
                .collect();
        }
    }

    let skills = Api::<KarsSkill>::namespaced(client.clone(), ns);
    let inherited = effective.spec.blueprint.clone();
    let team_policy = effective
        .spec
        .envelope
        .tool_policy_ref
        .as_ref()
        .map(|r| r.name.as_str());
    for role in &mut effective.spec.roster {
        if role.skills.is_empty() {
            continue;
        }
        let mut blueprint = role
            .blueprint
            .clone()
            .or_else(|| inherited.clone())
            .unwrap_or_default();
        let mut bound = blueprint.tool_policy.clone();
        for policy in [
            inherited.as_ref().and_then(|b| b.tool_policy.as_deref()),
            team_policy,
            role.envelope
                .as_ref()
                .and_then(|e| e.tool_policy_ref.as_ref())
                .map(|r| r.name.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            require_same_policy(&mut bound, policy, &role.name)?;
        }
        for name in &role.skills {
            let skill = skills
                .get_opt(name)
                .await?
                .ok_or_else(|| ReconcileError::Invalid(format!("skill '{name}' is missing")))?;
            let ready = skill.metadata.deletion_timestamp.is_none()
                && skill.metadata.generation.is_some()
                && skill.status.as_ref().is_some_and(|status| {
                    status.phase.as_deref() == Some(PHASE_READY)
                        && status.observed_generation == skill.metadata.generation
                        && status.version_digest.as_deref() == Some(skill.version_digest().as_str())
                });
            if !ready || !skill.validation_errors().is_empty() {
                return Err(ReconcileError::Invalid(format!(
                    "skill '{name}' is not currently Ready"
                )));
            }
            require_same_policy(&mut bound, &skill.spec.bounding_policy, &role.name)?;
            for server in skill.spec.mcp_servers {
                if !blueprint.mcp_servers.contains(&server) {
                    blueprint.mcp_servers.push(server);
                }
            }
            if let Some(recipe) = skill.spec.recipe {
                let instructions = blueprint.instructions.get_or_insert_with(String::new);
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&format!("[skill: {name}] {recipe}"));
            }
        }
        blueprint.tool_policy = bound;
        role.blueprint = Some(blueprint);
    }
    Ok(effective)
}

fn require_same_policy(
    bound: &mut Option<String>,
    policy: &str,
    role: &str,
) -> Result<(), ReconcileError> {
    if policy.trim().is_empty() || bound.as_ref().is_some_and(|existing| existing != policy) {
        return Err(ReconcileError::Invalid(format!(
            "role '{role}' has conflicting skill/team/envelope tool policy bounds"
        )));
    }
    *bound = Some(policy.into());
    Ok(())
}

pub(super) async fn capability_readiness(
    client: &Client,
    team: &KarsTeam,
) -> Result<(), ReconcileError> {
    let mut wanted = std::collections::BTreeSet::new();
    let blueprints = std::iter::once(team.spec.blueprint.clone()).chain(
        team.spec
            .roster
            .iter()
            .map(|role| specs::member_blueprint(team, role)),
    );
    for blueprint in blueprints.flatten() {
        wanted.extend(blueprint.mcp_servers);
    }
    let servers = Api::<McpServer>::namespaced(client.clone(), super::namespace(team)?);
    for name in wanted {
        let server = servers.get_opt(&name).await?.ok_or_else(|| {
            ReconcileError::Invalid(format!("MCP server '{name}' is not provisioned"))
        })?;
        if server.metadata.deletion_timestamp.is_some()
            || server.metadata.generation.is_none()
            || !server.status.as_ref().is_some_and(|status| {
                status.phase.as_deref() == Some(PHASE_READY)
                    && status.observed_generation == server.metadata.generation
            })
        {
            return Err(ReconcileError::Invalid(format!(
                "MCP server '{name}' is not currently Ready"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_skill_bound_must_agree() {
        let mut bound = None;
        require_same_policy(&mut bound, "read-only", "reader").unwrap();
        require_same_policy(&mut bound, "read-only", "reader").unwrap();
        assert!(require_same_policy(&mut bound, "write-all", "reader").is_err());
        assert!(require_same_policy(&mut bound, "", "reader").is_err());
    }
}
