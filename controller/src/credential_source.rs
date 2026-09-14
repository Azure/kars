// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public, opt-in agent credential-source contract. This is not a generic
//! Secret reference: the reserved name, purpose, target, and UID are mandatory.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSourceRef {
    #[schemars(
        length(min = 1, max = 253),
        regex(pattern = "^kars-credential-(source|bundle)-[a-z0-9][a-z0-9-]*$")
    )]
    pub name: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = "^[A-Za-z0-9-]+$"))]
    pub uid: String,
}

pub const SOURCE_PREFIX: &str = "kars-credential-source-";
pub const PURPOSE: &str = "kars.azure.com/credential-purpose";
pub const TARGET: &str = "kars.azure.com/credential-target";
pub const INTENT: &str = "kars.azure.com/credential-binding-intent";
pub const SANDBOX_UID: &str = "kars.azure.com/credential-sandbox-uid";
pub const WORKSPACE: &str = "kars.azure.com/credential-workspace";
pub const NAMESPACE_UID: &str = "kars.azure.com/credential-namespace-uid";
pub const SOURCE_UID: &str = "kars.azure.com/credential-source-uid";
pub const PROJECTION_UID: &str = "kars.azure.com/credential-projection-uid";
pub const SOURCE_PURPOSE: &str = "agent-source-v1";
pub const PROJECTION_PURPOSE: &str = "agent-projection-v1";
pub const BINDING_INTENT: &str = "explicit-reference-v1";
pub const POD_VERSION: &str = "kars.azure.com/credential-projection-version";

// Same agent-only allowlist as the existing handoff credential transport.
// OPENAI_API_KEY and other inference/control-plane variables are deliberately
// absent; those remain router/provider configuration, not agent credentials.
pub const AGENT_KEYS: &[&str] = &[
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_ALLOW_FROM",
    "SLACK_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "WHATSAPP_ENABLED",
    "BRAVE_API_KEY",
    "TAVILY_API_KEY",
    "EXA_API_KEY",
    "FIRECRAWL_API_KEY",
    "PERPLEXITY_API_KEY",
];

pub fn source_name(sandbox: &str) -> String {
    format!("{SOURCE_PREFIX}{sandbox}")
}

pub fn projection_name(sandbox: &str) -> String {
    format!("{sandbox}-credential-projection")
}

pub fn valid_ref(reference: &CredentialSourceRef, sandbox: &str) -> bool {
    reference.name == source_name(sandbox)
        && !reference.uid.is_empty()
        && reference.uid.len() <= 128
        && reference
            .uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::KarsSandbox;
    use kube::CustomResourceExt;

    #[test]
    fn reference_identity_is_retained_in_serialized_spec_digests() {
        let mut spec = crate::crd::KarsSandboxSpec {
            credentials_ref: Some(CredentialSourceRef {
                name: source_name("demo"),
                uid: "source-a".into(),
            }),
            ..Default::default()
        };
        let first = crate::providers::signing::sha256_hex(&serde_json::to_vec(&spec).unwrap());
        spec.credentials_ref.as_mut().unwrap().uid = "source-b".into();
        let second = crate::providers::signing::sha256_hex(&serde_json::to_vec(&spec).unwrap());
        assert_ne!(first, second);
    }

    #[test]
    fn reserved_reference_is_uid_pinned_and_not_an_arbitrary_secret_path() {
        let mut reference = CredentialSourceRef {
            name: source_name("demo"),
            uid: "source-uid".into(),
        };
        assert!(valid_ref(&reference, "demo"));
        assert!(!valid_ref(&reference, "other"));
        reference.name = "controller-receipt-identity".into();
        assert!(!valid_ref(&reference, "demo"));
        reference.name = source_name("demo");
        reference.uid = "../source".into();
        assert!(!valid_ref(&reference, "demo"));
    }

    #[test]
    fn generated_schema_keeps_legacy_specs_unchanged_and_requires_both_reference_fields() {
        let spec = crate::crd::KarsSandboxSpec::default();
        assert!(
            serde_json::to_value(spec)
                .unwrap()
                .get("credentialsRef")
                .is_none()
        );
        let crd = serde_json::to_value(KarsSandbox::crd()).unwrap();
        let schema = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"];
        let required = schema["properties"]["credentialsRef"]["required"]
            .as_array()
            .unwrap();
        assert!(required.contains(&serde_json::json!("name")));
        assert!(required.contains(&serde_json::json!("uid")));
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("credentialsRef"))
        );
    }
}
