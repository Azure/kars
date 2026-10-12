// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    kars_task::{KarsTaskSpec, TaskBlueprint, TaskEnvelope},
    kars_team::KarsTeam,
    kars_team_reconciler::specs,
};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Authority {
    blueprint: Option<TaskBlueprint>,
    envelope: TaskEnvelope,
}

pub(crate) fn seal(team: &KarsTeam, task: &mut KarsTask) -> Result<()> {
    let Some(plan) = plan(task) else {
        return Ok(());
    };
    let mut roles = BTreeMap::new();
    for reviewed in &plan.roles {
        let matches = team
            .spec
            .roster
            .iter()
            .filter(|role| role.name == reviewed.name)
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1,
            "reviewed role {:?} requires exactly one Team roster entry",
            reviewed.name
        );
        let spec = specs::member_spec(team, matches[0]);
        roles.insert(
            reviewed.name.clone(),
            Authority {
                blueprint: spec.blueprint,
                envelope: spec.envelope,
            },
        );
    }
    let binding = serde_json::to_string(&roles)?;
    ensure!(
        binding.len() <= 64 * 1024,
        "reviewed Team roster authority exceeds 64KiB"
    );
    task.annotations_mut().insert(TEAM_ROLES.into(), binding);
    Ok(())
}

fn binding(root: &KarsTask) -> Result<Option<BTreeMap<String, Authority>>> {
    let team_owned = root
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| {
            owners.len() == 1
                && owners[0].kind == "KarsTeam"
                && owners[0].api_version == "kars.azure.com/v1alpha1"
                && owners[0].controller == Some(true)
                && !owners[0].uid.is_empty()
        });
    let Some(raw) = ann(root, TEAM_ROLES) else {
        ensure!(
            !team_owned,
            "reviewed Team run lacks sealed roster authority; request a new run"
        );
        return Ok(None);
    };
    ensure!(
        team_owned,
        "reviewed roster authority requires exact Team ownership"
    );
    crate::kars_team_reconciler::requests::validate_work_run(root)?;
    ensure!(
        raw.len() <= 64 * 1024,
        "reviewed Team roster authority exceeds 64KiB"
    );
    let roles: BTreeMap<String, Authority> = serde_json::from_str(raw)?;
    let plan = plan(root).ok_or_else(|| anyhow::anyhow!("reviewed Team plan missing"))?;
    ensure!(
        roles.len() == plan.roles.len()
            && plan.roles.iter().all(|role| roles.contains_key(&role.name)),
        "sealed Team roster does not match the reviewed roles"
    );
    Ok(Some(roles))
}

pub(super) fn apply(root: &KarsTask, index: usize, spec: &mut KarsTaskSpec) -> Result<()> {
    let Some(mut roles) = binding(root)? else {
        return Ok(());
    };
    let role = &plan(root).unwrap().roles[index];
    let authority = roles.remove(&role.name).unwrap();
    let depth = spec.envelope.delegation_depth;
    spec.envelope = authority.envelope;
    spec.envelope.delegation_depth = spec.envelope.delegation_depth.min(depth);
    spec.blueprint = Some(authority.blueprint.unwrap_or_default());
    if let Some(execution) = &mut spec.execution {
        execution.runtime = None;
    }
    ensure!(
        matches!(
            crate::kars_task::blueprint::effective_runtime_name(spec),
            "OpenClaw" | "Hermes"
        ),
        "reviewed Team role {:?} requires an installed OpenClaw or Hermes adapter",
        role.name
    );
    ensure!(
        crate::kars_task::spec_attenuation_violations(spec, &root.spec).is_empty(),
        "reviewed Team role {:?} amplifies the standing run authority",
        role.name
    );
    Ok(())
}

pub(super) fn validate(root: &KarsTask) -> Result<()> {
    if binding(root)?.is_some() {
        for index in 0..plan(root).unwrap().roles.len() {
            let mut spec = root.spec.clone();
            spec.envelope.delegation_depth -= 1;
            apply(root, index, &mut spec)?;
        }
    }
    Ok(())
}
