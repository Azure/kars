// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared I/O helpers for the principles.md §3 "Ready ⇔ router echo"
//! loop, factored out of [`crate::tool_policy_reconciler`] so that
//! every CRD whose `spec` is loaded by the per-sandbox inference
//! router (ToolPolicy → AGT profile, InferencePolicy, KarsMemory,
//! McpServer egress, …) shares one implementation of:
//!
//! 1. Discovery — enumerate the `KarsSandbox`es that reference *this*
//!    policy CR via a caller-supplied filter predicate.
//! 2. Auth bootstrap — read each sandbox's
//!    `Secret kars-<sandbox>/router-admin-token` (key `token`).
//! 3. Confirmation poll — `GET /internal/policy-status` on the
//!    per-sandbox router service and aggregate the per-sandbox
//!    outcomes for [`crate::status::router_confirmation`]'s pure
//!    `decide_enforcement_state` aggregator.
//!
//! The functions here are deliberately split from the wire-contract
//! types + pure HTTP poller in
//! [`crate::status::router_confirmation`] so that the latter remains
//! free of `kube::Client` dependencies and stays unit-testable with
//! `wiremock` alone. This module adds the k8s glue.
//!
//! ## Generic over filter, not over CRD type
//!
//! `list_sandboxes_matching` takes a `FnMut(&KarsSandbox) -> bool`
//! filter rather than a hard-coded `tool_policy_name: &str` because
//! every reconciler binds to a different field on
//! `KarsSandbox.spec`:
//!
//! * ToolPolicy → `spec.governance.toolPolicyRef.name`
//! * InferencePolicy → `spec.inferenceRef`
//! * KarsMemory → `spec.memoryRef` (future)
//! * McpServer fleet → `spec.mcpServerRefs[*].name` (future)
//!
//! Keeping the predicate on the caller side preserves the
//! "single-responsibility" principle for the helper without baking in
//! a CRD-kind enum that would need to grow with every new consumer.

use crate::crd::KarsSandbox;
use crate::status::router_confirmation::{
    self, ConfirmError, fetch_router_policy_status, router_admin_url,
};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams},
};

/// Enumerate `KarsSandbox`es in `ns` that satisfy `matches`.
///
/// Returns the bare sandbox `metadata.name` values. The controller
/// convention is one router-service per sandbox at
/// `{name}.kars-{name}.svc.cluster.local:8443`, so the
/// *router's* namespace is `kars-<name>`, distinct from `ns`
/// (which is typically `kars-system`, where the policy CRs
/// live).
///
/// `matches` runs against each candidate `KarsSandbox`. Typical
/// shape from a reconciler:
///
/// ```ignore
/// list_sandboxes_matching(client, "kars-system", |cs| {
///     cs.spec
///         .governance
///         .as_ref()
///         .is_some_and(|g| g.tool_policy_ref.name == policy_name)
/// })
/// .await?;
/// ```
pub async fn list_sandboxes_matching<F>(
    client: &Client,
    ns: &str,
    mut matches: F,
) -> Result<Vec<String>, kube::Error>
where
    F: FnMut(&KarsSandbox) -> bool,
{
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), ns);
    let list = api.list(&ListParams::default()).await?;
    Ok(list
        .items
        .into_iter()
        .filter(|cs| matches(cs))
        .map(|cs| cs.name_any())
        .collect())
}

/// Read the per-sandbox admin token from
/// `Secret kars-<sandbox>/router-admin-token` (key `token`).
///
/// `Ok(None)` is returned when the Secret or the `token` key is not
/// yet present — the reconciler treats that as a transient
/// awaiting-router condition rather than a hard failure (the
/// sandbox reconciler may not yet have completed its first pass).
/// An empty token string is folded into `Ok(None)` for the same
/// reason.
pub async fn read_admin_token(
    client: &Client,
    sandbox: &str,
) -> Result<Option<String>, kube::Error> {
    let secret_ns = format!("kars-{sandbox}");
    let api: Api<Secret> = Api::namespaced(client.clone(), &secret_ns);
    let secret = match api.get_opt("router-admin-token").await? {
        Some(s) => s,
        None => return Ok(None),
    };
    Ok(secret
        .data
        .as_ref()
        .and_then(|d| d.get("token"))
        .and_then(|v| String::from_utf8(v.0.clone()).ok())
        .filter(|t| !t.is_empty()))
}

/// Poll every sandbox in `sandboxes` once and assemble the per-
/// sandbox outcome list consumed by
/// [`router_confirmation::decide_enforcement_state`].
///
/// A sandbox whose admin-token Secret is missing or unreadable is
/// recorded as `Err(`[`ConfirmError::HttpStatus`]`(0))` — the `0`
/// sentinel is distinct from any real HTTP status code so it stays
/// disambiguable in operator logs while still aggregating as
/// "router unreachable" in the Ready=False detail message.
///
/// Polls run sequentially (one sandbox at a time). This is the
/// right shape for the current scale (≤O(10) sandboxes per policy
/// in practice); contention concerns belong to a future watch-based
/// rewrite, not this helper.
pub async fn poll_referencing_sandboxes(
    client: &Client,
    http: &reqwest::Client,
    sandboxes: &[String],
) -> Vec<(
    String,
    Result<router_confirmation::PolicyStatusResponse, ConfirmError>,
)> {
    let mut out = Vec::with_capacity(sandboxes.len());
    for sandbox in sandboxes {
        let token = match read_admin_token(client, sandbox).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                out.push((sandbox.clone(), Err(ConfirmError::HttpStatus(0))));
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    sandbox = %sandbox,
                    error = %e,
                    "router-admin-token Secret read failed"
                );
                out.push((sandbox.clone(), Err(ConfirmError::HttpStatus(0))));
                continue;
            }
        };
        let url = router_admin_url(sandbox);
        let r = fetch_router_policy_status(http, &url, &token).await;
        out.push((sandbox.clone(), r));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::crd::{KarsSandbox, KarsSandboxSpec};
    use kube::core::ObjectMeta;

    fn mk_sandbox(name: &str, tp_ref: Option<&str>) -> KarsSandbox {
        let mut cs = KarsSandbox {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: KarsSandboxSpec::default(),
            status: None,
        };
        if let Some(tp) = tp_ref {
            cs.spec.governance = Some(crate::crd::GovernanceConfig {
                tool_policy_ref: crate::mcp_server::LocalObjectRef { name: tp.into() },
                ..Default::default()
            });
        }
        cs
    }

    #[test]
    fn filter_predicate_runs_against_each_item() {
        // We can't easily run list_sandboxes_matching against a fake
        // kube API in unit tests (it would need a httpmock-shaped
        // kube fixture). Instead pin the predicate-shape we
        // promise to callers so future refactors don't drift.
        let cs_a = mk_sandbox("a", Some("prod-tools"));
        let cs_b = mk_sandbox("b", Some("dev-tools"));
        let cs_c = mk_sandbox("c", None);

        let pred = |cs: &KarsSandbox| {
            cs.spec
                .governance
                .as_ref()
                .is_some_and(|g| g.tool_policy_ref.name == "prod-tools")
        };

        assert!(pred(&cs_a));
        assert!(!pred(&cs_b));
        assert!(!pred(&cs_c));
    }
}
