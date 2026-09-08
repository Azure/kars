// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata-only operator delegation. Credential values remain native Secrets.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const NAME: &str = "workspace";
pub const INPUT_PREFIX: &str = "kars-credential-input-";
pub const BUNDLE_PREFIX: &str = "kars-credential-bundle-";
pub const INPUT_PURPOSE: &str = "agent-input-v2";
pub const BUNDLE_PURPOSE: &str = "agent-bundle-v2";
pub const GRANT_UID: &str = "kars.azure.com/credential-grant-uid";
pub const TARGET_KIND: &str = "kars.azure.com/credential-target-kind";
pub const TARGET_UID: &str = "kars.azure.com/credential-target-uid";
pub const GRANT_OWNER: &str = "kars.azure.com/credential-grant-owner";
pub const INPUT_STATE: &str = "kars.azure.com/credential-input-state";

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ObjectIdentity {
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialTarget {
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialWriter {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "camelCase")]
pub enum CredentialScope {
    Workspace,
    Team,
    Target,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSelection {
    pub scope: CredentialScope,
    pub source: ObjectIdentity,
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<CredentialTarget>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialBindings {
    pub grant: ObjectIdentity,
    pub sources: Vec<CredentialSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationStore {
    pub secret: ObjectIdentity,
    pub purpose: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BridgeConsumers {
    pub bff: ObjectIdentity,
    pub gateway: ObjectIdentity,
    #[serde(default = "one")]
    pub gateway_replicas: i32,
}
fn one() -> i32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LegacyImport {
    pub source_name: String,
    pub namespace: String,
    pub namespace_uid: String,
    pub secret: ObjectIdentity,
    pub resource_version: String,
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<CredentialTarget>,
}

#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsCredentialGrant",
    plural = "karscredentialgrants",
    namespaced,
    status = "CredentialGrantStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct KarsCredentialGrantSpec {
    pub workspace_uid: String,
    pub writers: Vec<CredentialWriter>,
    #[serde(default)]
    pub agent_keys: Vec<String>,
    #[serde(default)]
    pub integration_stores: Vec<IntegrationStore>,
    #[serde(default)]
    pub legacy_imports: Vec<LegacyImport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<ObjectIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_consumers: Option<BridgeConsumers>,
    #[serde(default)]
    pub router_operator_access: bool,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceMetadata {
    pub name: String,
    pub uid: String,
    pub resource_version: String,
    pub keys: Vec<String>,
    pub phase: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<CredentialTarget>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CredentialGrantStatus {
    pub observed_generation: i64,
    pub phase: String,
    pub reason: String,
    #[serde(default)]
    pub sources: Vec<SourceMetadata>,
    #[serde(default)]
    pub legacy_sources: Vec<LegacyImport>,
    #[serde(default)]
    pub conditions: Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_revision: Option<String>,
}

pub fn agent_key(key: &str) -> bool {
    if key.is_empty()
        || key.len() > 128
        || key.as_bytes()[0].is_ascii_digit()
        || !key
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return false;
    }
    let forbidden_prefixes = [
        "AGT_",
        "AZURE_",
        "IMDS_",
        "KARS_",
        "FOUNDRY_",
        "KUBERNETES_",
        "LD_",
        "DYLD_",
        "NODE_",
        "PYTHON",
        "BASH",
        "ENV_",
        "SSL_",
        "RUST_",
        "CARGO_",
        "GIT_",
        "SSH_",
        "OPENAI_",
        "ANTHROPIC_",
        "GEMINI_",
        "GOOGLE_",
        "OLLAMA_",
        "COPILOT_",
    ];
    if forbidden_prefixes
        .iter()
        .any(|prefix| key.starts_with(prefix))
    {
        return false;
    }
    if [
        "PATH",
        "HOME",
        "SHELL",
        "ENV",
        "IFS",
        "USER",
        "LOGNAME",
        "PWD",
        "TMPDIR",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "COPILOT_GITHUB_TOKEN",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    ]
    .contains(&key)
    {
        return false;
    }
    crate::credential_source::AGENT_KEYS.contains(&key)
        || [
            "_TOKEN",
            "_KEY",
            "_SECRET",
            "_PASSWORD",
            "_PAT",
            "_CREDENTIAL",
            "_CREDENTIALS",
            "_CONNECTION_STRING",
            "_AUTH",
            "_AUTHORIZATION",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}

pub fn permitted_agent_keys(grant: &KarsCredentialGrant) -> Result<Vec<String>, String> {
    let mut keys = crate::credential_source::AGENT_KEYS
        .iter()
        .map(|key| key.to_string())
        .collect::<Vec<_>>();
    for key in &grant.spec.agent_keys {
        if !agent_key(key) {
            return Err("Grant contains a reserved or invalid agent key".into());
        }
        keys.push(key.clone());
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

pub fn validate_bindings(bindings: &CredentialBindings) -> Result<(), String> {
    if bindings.grant.name != NAME
        || bindings.grant.uid.is_empty()
        || bindings.sources.is_empty()
        || bindings.sources.len() > 3
    {
        return Err(
            "Credential bindings require the exact workspace grant and one to three sources".into(),
        );
    }
    let mut prior = None;
    for selection in &bindings.sources {
        if !selection.source.name.starts_with(INPUT_PREFIX)
            || selection.source.uid.is_empty()
            || selection.keys.iter().any(|key| !agent_key(key))
            || prior.is_some_and(|scope| scope >= selection.scope)
        {
            return Err("Credential sources must be unique and ordered workspace, Team, target with explicit safe key grants".into());
        }
        if selection.scope == CredentialScope::Team
            && selection.owner.as_ref().is_none_or(|owner| {
                owner.kind != "KarsTeam"
                    || owner.uid.is_empty()
                    || owner.namespace.is_empty()
                    || owner.name.is_empty()
            })
        {
            return Err("Team credential inheritance requires the exact Team identity".into());
        }
        prior = Some(selection.scope);
    }
    Ok(())
}

pub fn attenuates(child: Option<&CredentialBindings>, parent: Option<&CredentialBindings>) -> bool {
    let Some(child) = child else { return true };
    let Some(parent) = parent else { return false };
    child.grant == parent.grant
        && child.sources.iter().all(|source| {
            parent.sources.iter().any(|bound| {
                source.scope == bound.scope
                    && source.source == bound.source
                    && source.owner == bound.owner
                    && source.keys.iter().all(|key| bound.keys.contains(key))
            })
        })
}

pub fn integration_keys(purpose: &str, name: &str, key: &str) -> bool {
    match purpose {
        "providers" if name == "kars-inference-providers" => {
            key == "COPILOT_GITHUB_TOKEN"
                || (key.starts_with("KARS_PROVIDER_")
                    && ["_ENDPOINT", "_API_KEY", "_TOKEN", "_MODELS"]
                        .iter()
                        .any(|suffix| key.ends_with(suffix)))
        }

        "foundry" if name == "kars-foundry-credentials" => key == "FOUNDRY_API_KEY",
        "provider-default" if name.starts_with("kars-provider-") => key == "API_KEY",
        "github-app" if name == "kars-github-app" => {
            ["GITHUB_APP_ID", "GITHUB_APP_PRIVATE_KEY"].contains(&key)
        }
        "github-connection" if name == "kars-github-connection" => {
            ["GITHUB_TOKEN", "GITHUB_OWNER", "GITHUB_REPO"].contains(&key)
        }
        "teams" => [
            "client-id",
            "tenant-id",
            "client-secret",
            "entra-role-map",
            "bff-internal-secret",
        ]
        .contains(&key),
        "controller-settings" if name == "kars-credential-controller-settings" => {
            key == "configuration"
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "credential_grant_tests.rs"]
mod tests;
