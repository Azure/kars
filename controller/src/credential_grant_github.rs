// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{CredentialBindings, GitHubBinding, NAME};

pub fn repository(value: &str) -> bool {
    let Some((owner, repo)) = value.split_once('/') else {
        return false;
    };
    let part = |part: &str, max: usize| {
        !part.is_empty()
            && part.len() <= max
            && ![".", ".."].contains(&part)
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
    };
    part(owner, 39) && part(repo, 100)
}

pub fn validate(binding: &GitHubBinding) -> Result<(), String> {
    if binding.grant.name != NAME
        || binding.grant.uid.is_empty()
        || !binding
            .connection
            .name
            .starts_with("kars-github-connection-")
        || binding.connection.uid.is_empty()
        || binding.repositories.is_empty()
        || binding.repositories.len() > 32
        || binding.repositories.iter().any(|repo| !repository(repo))
        || binding
            .repositories
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != binding.repositories.len()
    {
        return Err("Keyless GitHub requires a UID-bound operator grant/connection and 1–32 canonical repositories".into());
    }
    Ok(())
}

pub fn attenuates(child: Option<&GitHubBinding>, parent: Option<&GitHubBinding>) -> bool {
    let Some(child) = child else { return true };
    let Some(parent) = parent else { return false };
    child.grant == parent.grant
        && child.connection == parent.connection
        && (!child.write || parent.write)
        && child
            .repositories
            .iter()
            .all(|repo| parent.repositories.contains(repo))
}

pub fn agent_sources(bindings: Option<&CredentialBindings>) -> Result<(), String> {
    let bindings=bindings.ok_or("Keyless GitHub requires explicit governed agent sources; legacy direct credentials are not implicitly migrated")?;
    super::validate_bindings(bindings)?;
    if bindings
        .sources
        .iter()
        .flat_map(|source| &source.keys)
        .any(|key| !crate::credential_source::AGENT_KEYS.contains(&key.as_str()))
    {
        return Err("Keyless GitHub cannot be combined with raw GitHub or custom agent credentials without a separately reviewed purpose contract".into());
    }
    Ok(())
}

pub fn opaque_github_egress(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "*"
        || ["github.com", "api.github.com"].iter().any(|target| {
            host == *target
                || host.strip_prefix("*.").is_some_and(|suffix| {
                    *target == suffix || target.ends_with(&format!(".{suffix}"))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_grant::ObjectIdentity;

    fn binding() -> GitHubBinding {
        GitHubBinding {
            grant: ObjectIdentity {
                name: NAME.into(),
                uid: "grant".into(),
            },
            connection: ObjectIdentity {
                name: "kars-github-connection-test".into(),
                uid: "connection".into(),
            },
            repositories: vec!["owner/repo".into()],
            write: false,
        }
    }
    #[test]
    fn governed_github_bindings_reject_alias_paths_and_attenuate_repositories_write_and_uids() {
        let parent = binding();
        assert!(validate(&parent).is_ok());
        assert!(attenuates(Some(&parent), Some(&parent)));
        for repo in [
            "Owner/repo",
            "owner/../repo",
            "owner/repo.git/extra",
            "owner/%2e",
            "owner/..",
            "owner/",
        ] {
            let mut child = parent.clone();
            child.repositories = vec![repo.into()];
            assert!(validate(&child).is_err(), "{repo}");
        }
        for changed in ["uid", "grant", "repo", "write"] {
            let mut child = parent.clone();
            match changed {
                "uid" => child.connection.uid = "replacement".into(),
                "grant" => child.grant.uid = "replacement".into(),
                "repo" => child.repositories = vec!["owner/foreign".into()],
                _ => child.write = true,
            }
            assert!(!attenuates(Some(&child), Some(&parent)), "{changed}");
        }
    }
    #[test]
    fn governed_github_rejects_opaque_api_egress_and_implicit_legacy_credentials() {
        for host in [
            "github.com",
            "api.github.com",
            "*.github.com",
            "*.com",
            "*",
            "GITHUB.COM.",
        ] {
            assert!(opaque_github_egress(host), "{host}");
        }
        assert!(!opaque_github_egress("docs.example.com"));
        assert!(agent_sources(None).is_err());
    }
    #[test]
    fn governed_github_selection_is_part_of_the_existing_full_task_authorization_digest() {
        let model = crate::kars_task::TaskModel {
            provider: "test".into(),
            deployment: "test".into(),
        };
        let mut task = crate::kars_task::KarsTaskSpec {
            blueprint: Some(crate::kars_task::TaskBlueprint {
                github_binding: Some(binding()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let original = task.authorization_digest_with_model(&model);
        assert_eq!(
            task.authorization_configuration_with_model(&model)["blueprint"]["githubBinding"]["connection"]
                ["uid"],
            "connection"
        );
        task.blueprint
            .as_mut()
            .unwrap()
            .github_binding
            .as_mut()
            .unwrap()
            .connection
            .uid = "replacement".into();
        assert_ne!(task.authorization_digest_with_model(&model), original);
    }
}
