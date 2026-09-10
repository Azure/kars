// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::mcp_server::{McpOAuthConfig, McpServer, McpServerSpec};
use kube::ResourceExt;

pub(super) async fn resolve_mcp_source(
    mcp: &McpServer,
) -> (
    McpServerSpec,
    Option<String>,
    Option<(&'static str, String)>,
) {
    let spec = &mcp.spec;
    if spec.managed.is_some() {
        return match super::managed::validate_source(mcp) {
            Ok(()) => (spec.clone(), None, None),
            Err(error) => (spec.clone(), None, Some(("InvalidSpec", error))),
        };
    }
    let inline_any = spec.url.is_some()
        || spec.oauth.is_some()
        || spec.production_mode.is_some()
        || spec.scopes.is_some()
        || spec.allowed_tools.is_some()
        || spec.display_name.is_some();
    if inline_any && spec.bundle_ref.is_some() {
        return (
            McpServerSpec {
                allowed_sandboxes: spec.allowed_sandboxes.clone(),
                ..Default::default()
            },
            None,
            Some((
                "InvalidSpec",
                "spec.bundleRef is mutually exclusive with inline content fields".into(),
            )),
        );
    }
    let Some(bundle_ref) = spec.bundle_ref.as_ref() else {
        return (spec.clone(), None, None);
    };
    let signer_policy_handle = crate::signer_policy::global();
    let verify_result = match signer_policy_handle.snapshot() {
        crate::signer_policy::SignerPolicyState::FromConfigMap(p) => {
            let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::mcp_server::McpServerKind,
            >(bundle_ref, &cfg)
            .await
        }
        crate::signer_policy::SignerPolicyState::Malformed(message) => Err(
            crate::policy_fetcher::FetchError::SignerPolicyMalformed(message),
        ),
        crate::signer_policy::SignerPolicyState::Absent => {
            let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::mcp_server::McpServerKind,
            >(bundle_ref, &cfg)
            .await
        }
    };
    match verify_result {
        Ok(verified) => (
            merge_bundle_with_selector(spec, &verified),
            Some(verified.digest),
            None,
        ),
        Err(error) => {
            let reason = crate::policy_fetcher::reason_for_error(&error).unwrap_or("Transient");
            tracing::warn!(mcpserver = %mcp.name_any(), reason, "McpServer bundle verification failed");
            (
                McpServerSpec {
                    allowed_sandboxes: spec.allowed_sandboxes.clone(),
                    ..Default::default()
                },
                None,
                Some((reason, error.to_string())),
            )
        }
    }
}

fn merge_bundle_with_selector(
    cr_spec: &McpServerSpec,
    verified: &crate::policy_canonical::mcp_server::VerifiedMcpServerBundle,
) -> McpServerSpec {
    let oauth = verified.oauth.as_ref().map(|oauth| McpOAuthConfig {
        issuer: oauth.issuer.clone(),
        audience: oauth.audience.clone(),
        resource: oauth.resource.clone(),
        pkce: oauth.pkce.clone().unwrap_or_else(|| "S256".into()),
    });
    McpServerSpec {
        url: verified.url.clone(),
        managed: None,
        oauth,
        production_mode: verified.production_mode,
        scopes: verified.scopes.clone(),
        allowed_tools: verified.allowed_tools.clone(),
        allowed_sandboxes: cr_spec.allowed_sandboxes.clone(),
        display_name: verified.display_name.clone(),
        bundle_ref: None,
        bearer_from_env: cr_spec.bearer_from_env.clone(),
    }
}
