// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn bindings() -> CredentialBindings {
    CredentialBindings {
        grant: ObjectIdentity {
            name: NAME.into(),
            uid: "grant-uid".into(),
        },
        sources: vec![CredentialSelection {
            scope: CredentialScope::Workspace,
            source: ObjectIdentity {
                name: format!("{INPUT_PREFIX}workspace"),
                uid: "source-uid".into(),
            },
            keys: vec!["GITHUB_TOKEN".into()],
            owner: None,
        }],
    }
}

#[test]
fn governed_credentials_reject_process_bootstrap_and_router_identity_keys() {
    for key in [
        "PATH",
        "HOME",
        "NODE_OPTIONS",
        "LD_PRELOAD",
        "PYTHONPATH",
        "BASH_ENV",
        "JAVA_TOOL_OPTIONS",
        "GIT_SSH_COMMAND",
        "KARS_ADMIN_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AZURE_CLIENT_SECRET",
        "AWS_SESSION_TOKEN",
        "COPILOT_GITHUB_TOKEN",
        "HTTP_PROXY",
    ] {
        assert!(!agent_key(key), "{key}");
    }
    for key in crate::credential_source::AGENT_KEYS {
        assert!(agent_key(key), "{key}");
    }
    assert!(agent_key("GITHUB_TOKEN"));
    assert!(agent_key("INTERNAL_SERVICE_SECRET"));
}

#[test]
fn governed_credentials_keep_legacy_defaults_and_require_explicit_custom_key_grants() {
    let grant = KarsCredentialGrant::new(
        NAME,
        KarsCredentialGrantSpec {
            workspace_uid: "workspace".into(),
            writers: vec![],
            agent_keys: vec![],
            integration_stores: vec![],
            legacy_imports: vec![],
            controller: None,
            bridge_consumers: None,
            observation_targets: Vec::new(),
            github_connections: Vec::new(),
            enabled: true,
        },
    );
    let keys = permitted_agent_keys(&grant).unwrap();
    assert_eq!(keys.len(), 10);
    assert!(!keys.contains(&"GITHUB_TOKEN".into()));
    let mut custom = grant.clone();
    custom.spec.agent_keys.push("GITHUB_TOKEN".into());
    assert!(
        permitted_agent_keys(&custom)
            .unwrap()
            .contains(&"GITHUB_TOKEN".into())
    );
    custom.spec.agent_keys.push("NODE_OPTIONS".into());
    assert!(permitted_agent_keys(&custom).is_err());
}

#[test]
fn governed_credentials_participate_in_the_shared_canonical_task_authority() {
    let model = crate::kars_task::TaskModel {
        provider: "azure-openai".into(),
        deployment: "test".into(),
    };
    let mut spec = crate::kars_task::KarsTaskSpec {
        blueprint: Some(crate::kars_task::TaskBlueprint {
            credential_bindings: Some(bindings()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let original = spec.authorization_digest_with_model(&model);
    let snapshot = spec.authorization_configuration_with_model(&model);
    assert_eq!(
        snapshot["blueprint"]["credentialBindings"]["sources"][0]["source"]["uid"],
        "source-uid"
    );
    for changed in ["source", "grant", "keys"] {
        spec.blueprint.as_mut().unwrap().credential_bindings = Some(bindings());
        let bindings = spec
            .blueprint
            .as_mut()
            .unwrap()
            .credential_bindings
            .as_mut()
            .unwrap();
        match changed {
            "source" => bindings.sources[0].source.uid = "replacement".into(),
            "grant" => bindings.grant.uid = "replacement".into(),
            _ => bindings.sources[0].keys.push("BRAVE_API_KEY".into()),
        }
        assert_ne!(
            original,
            spec.authorization_digest_with_model(&model),
            "{changed}"
        );
    }
}

#[test]
fn governed_credentials_attenuate_sources_grants_and_key_sets() {
    let parent = bindings();
    let mut child = parent.clone();
    assert!(attenuates(Some(&child), Some(&parent)));
    child.sources[0].keys.clear();
    assert!(validate_bindings(&child).is_ok());
    assert!(attenuates(Some(&child), Some(&parent)));
    child.sources[0].keys.push("BRAVE_API_KEY".into());
    assert!(!attenuates(Some(&child), Some(&parent)));
    child = parent.clone();
    child.sources[0].source.uid = "other".into();
    assert!(!attenuates(Some(&child), Some(&parent)));
    assert!(!attenuates(Some(&parent), None));
    assert!(attenuates(None, Some(&parent)));
}

#[test]
fn governed_credentials_preserve_order_and_do_not_use_arbitrary_secret_names() {
    let mut value = bindings();
    value.sources[0].source.name = "controller-receipt-identity".into();
    assert!(validate_bindings(&value).is_err());
    value = bindings();
    value.sources.push(value.sources[0].clone());
    assert!(validate_bindings(&value).is_err());
    assert!(!integration_keys(
        "providers",
        "controller-receipt-identity",
        "COPILOT_GITHUB_TOKEN"
    ));
    assert!(integration_keys(
        "provider-default",
        "kars-provider-existing-customer",
        "API_KEY"
    ));
    assert!(!integration_keys(
        "teams",
        "customer-teams",
        "session-secret"
    ));
}

#[test]
fn governed_credentials_attenuation_retains_effective_override_and_absent_key_masks() {
    for scope in [CredentialScope::Team, CredentialScope::Target] {
        let mut parent = bindings();
        parent.sources[0].keys.push("BRAVE_API_KEY".into());
        parent.sources.push(CredentialSelection {
            scope,
            source: ObjectIdentity {
                name: format!("{INPUT_PREFIX}later"),
                uid: "later".into(),
            },
            keys: vec!["GITHUB_TOKEN".into()],
            owner: Some(CredentialTarget {
                kind: if scope == CredentialScope::Team {
                    "KarsTeam"
                } else {
                    "KarsTask"
                }
                .into(),
                namespace: "work".into(),
                name: "owner".into(),
                uid: "owner-uid".into(),
            }),
        });
        // This is declaration-only: the same checks apply to a present later
        // value and to an absent later value that masks the workspace value.
        let mut child = parent.clone();
        child.sources.pop();
        assert!(!attenuates(Some(&child), Some(&parent)));
        child.sources[0].keys.retain(|key| key == "BRAVE_API_KEY");
        assert!(attenuates(Some(&child), Some(&parent)));
        child = parent.clone();
        child.sources[1].keys.clear();
        assert!(!attenuates(Some(&child), Some(&parent)));
        child.sources[0].keys.retain(|key| key == "BRAVE_API_KEY");
        assert!(attenuates(Some(&child), Some(&parent)));
        child = parent.clone();
        child.sources[0].keys.clear();
        assert!(attenuates(Some(&child), Some(&parent)));
        child.sources.swap(0, 1);
        assert!(!attenuates(Some(&child), Some(&parent)));
        assert!(!attenuates(Some(&parent), Some(&child)));
    }
}
