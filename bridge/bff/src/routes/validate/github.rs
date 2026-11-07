// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Check, CheckStatus};
use crate::auth::Principal;
use crate::kars::{cluster::Cluster, credential_contract::GitHubBinding, task::KarsTask};
use crate::routes::tasks::BlueprintDto;

pub(super) fn write_enabled(_workload: Option<&str>, blueprint: &BlueprintDto) -> bool {
    crate::routes::tasks::mission_github_write_enabled(Some(blueprint))
}

pub(super) async fn check<'a>(
    cluster: &Cluster,
    ns: &str,
    principal: &Principal,
    repositories: Option<&[String]>,
    write: bool,
    hosts: impl Iterator<Item = &'a str>,
    stored: Option<&GitHubBinding>,
) -> Vec<Check> {
    let selected =
        repositories.is_some_and(|repos| repos.iter().any(|repo| !repo.trim().is_empty()));
    if !selected && stored.is_none() {
        return vec![];
    }
    let authority =
        crate::routes::github::authorize_github_access(cluster, ns, principal, repositories, write)
            .await;
    let mut checks = vec![match authority {
        Ok(Some((_, binding))) if stored.is_none_or(|stored| same_binding(stored, &binding)) => {
            Check {
                id: "github_authority".into(),
                label: "Selected GitHub repositories".into(),
                status: CheckStatus::Pass,
                detail: format!(
                    "Your GitHub connection and the workspace's current approval cover {} access to: {}. Permissions will be checked again when creating or starting the work.",
                    if write { "pull-request" } else { "read-only" },
                    binding.repositories.join(", ")
                ),
            }
        }
        _ => Check {
            id: "github_authority".into(),
            label: "Selected GitHub repositories need approval".into(),
            status: CheckStatus::Fail,
            detail: format!(
                "Kars could not confirm {} access to the selected repositories. Open Configuration → GitHub and check that these repositories are connected for your account. If they are already connected, ask your workspace administrator to approve them for this workspace with {} permission, then check readiness again. Connecting GitHub alone does not grant workspace approval.{}",
                if write { "pull-request" } else { "read-only" },
                if write { "pull-request" } else { "read-only" },
                if stored.is_some() {
                    " If the connection or workspace approval was replaced, create a new draft so it uses the current approval; this saved draft was not changed."
                } else {
                    ""
                }
            ),
        },
    }];
    if let Err(error) = crate::routes::github::validate_bound_github_egress(hosts) {
        checks.push(Check {
            id: "github_egress".into(),
            label: "GitHub network settings conflict".into(),
            status: CheckStatus::Fail,
            detail: error.to_string(),
        });
    }
    checks
}

fn same_binding(stored: &GitHubBinding, live: &GitHubBinding) -> bool {
    stored.grant == live.grant
        && stored.connection == live.connection
        && stored.write == live.write
        && stored.repositories.len() == live.repositories.len()
        && stored
            .repositories
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            == live
                .repositories
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
}

pub(super) async fn check_stored(
    cluster: &Cluster,
    ns: &str,
    principal: &Principal,
    task: &KarsTask,
) -> Vec<Check> {
    let Some(blueprint) = task.spec.blueprint.as_ref() else {
        return vec![];
    };
    let Some(binding) = blueprint.github_binding.as_ref() else {
        return if blueprint.git_write.is_some() {
            vec![Check {
                id: "github_authority".into(),
                label: "Saved GitHub access needs a new draft".into(),
                status: CheckStatus::Fail,
                detail: "This saved draft has legacy GitHub access without a protected workspace approval. Create a new draft, select the repositories again and check readiness. The saved draft was not changed.".into(),
            }]
        } else {
            vec![]
        };
    };
    check(
        cluster,
        ns,
        principal,
        Some(&binding.repositories),
        binding.write,
        blueprint.egress.iter().map(|entry| entry.host.as_str()),
        Some(binding),
    )
    .await
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
