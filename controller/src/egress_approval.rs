// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `EgressApproval` CRD — Slice 5e-thin of `crd-well-oiled-machine`.
//!
//! The grant lane (per `principles.md §2.4` and
//! `docs/internal/crd-well-oiled-machine/signing-model.md §6`).
//!
//! ## What it is
//!
//! A short-TTL, scoped, attributable, **ephemeral** widening of the
//! baseline egress allowlist for a single sandbox. The baseline lives
//! in `ClawSandbox.spec.networkPolicy.allowlistRef` (a cosign-signed
//! OCI artifact, Slice 1c.1). An `EgressApproval` adds a small list of
//! hosts on top of that baseline for the duration declared in
//! `spec.ttl`, then auto-expires. Approvals **never** migrate into the
//! signed bundle automatically — operators who want a hostname
//! permanent re-sign the bundle the normal way.
//!
//! ## Authority model (thin)
//!
//! K8s RBAC is the authority. Binding the
//! `azureclaw:egress-approver` ClusterRole grants `create / get /
//! list / delete` on this CRD; the k8s audit log records who used it.
//!
//! Slice 5e+ (deferred, demand-gated) layers an optional cryptographic
//! attestation on top — an ed25519 signature over the canonical
//! approval bytes, verified against the controller-side
//! `SignerPolicy.spec.ed25519Keys[]` registry that landed in Slice
//! 1c.6. The CRD shape here is forward-compatible (no schema change
//! required when the attestation field is added).
//!
//! ## Resolution invariants
//!
//! 1. `spec.sandbox` is a `ClawSandbox` in the **same namespace** as
//!    the approval. Cross-namespace approvals are explicitly out of
//!    scope; one approval = one sandbox.
//! 2. `spec.hosts.length in 1..=16` — small, scoped grants only.
//! 3. `spec.reason.length in 1..=512` — non-empty, audit-grade text.
//! 4. `spec.ttl` parses as a positive ISO 8601 duration ≤ the
//!    cluster's `maxApprovalTtl` Helm value (default 24h, hard ceiling
//!    7d). The reconciler enforces the upper bound at admission time
//!    in addition to CEL.
//!
//! ## Lifecycle
//!
//! `Pending` (admitted, sibling sandbox not yet Ready) →
//! `Active` (router echoed the merged digest) →
//! `Expired` (TTL elapsed; mount dropped; router echoed the baseline
//! digest again).
//!
//! Deletion at any phase is honoured via the finalizer; the reconciler
//! drops the approval's mount file, waits for the router to echo the
//! post-removal merged digest, then removes the finalizer.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::EndpointConfig;

/// `EgressApproval.spec` — declares a temporary, scoped widening of a
/// sandbox's baseline egress allowlist.
///
/// The CR is namespaced; `spec.sandbox` MUST refer to a `ClawSandbox`
/// in the same namespace.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "EgressApproval",
    namespaced,
    status = "EgressApprovalStatus",
    shortname = "eappr",
    printcolumn = r#"{"name":"Sandbox","type":"string","jsonPath":".spec.sandbox"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Expires","type":"string","jsonPath":".status.expiresAt"}"#,
    printcolumn = r#"{"name":"Hosts","type":"integer","jsonPath":".status.hostCount"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct EgressApprovalSpec {
    /// Sandbox name (within the same namespace as this CR). The
    /// reconciler resolves the `ClawSandbox`, fails admission if
    /// absent, and waits for `phase=Ready` before promoting the
    /// approval to `Active`.
    pub sandbox: String,

    /// Hosts to grant on top of the baseline. Same canonical form as
    /// `ClawSandbox.spec.networkPolicy.allowedEndpoints`. CEL bound:
    /// 1..=16 entries per approval. Per-entry CEL is delegated to the
    /// `EndpointConfig` schema (host non-empty, port 1..=65535).
    pub hosts: Vec<EndpointConfig>,

    /// Human-readable reason. CEL: 1..=512 chars; the reconciler also
    /// rejects control bytes as a defense-in-depth check. Surfaced in
    /// audit log + CLI listing.
    pub reason: String,

    /// Optional ticket / incident reference (e.g. `INC-12345`). Not
    /// validated for shape; surfaced in audit log only. CEL: when set,
    /// 1..=128 chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,

    /// Time-to-live. ISO 8601 duration form (`PT15M`, `PT4H`, `P1D`).
    /// CEL: must parse and be > 0; the reconciler enforces the
    /// cluster ceiling (`maxApprovalTtl`, default 24h, hard 7d).
    pub ttl: String,
}

/// Status of an `EgressApproval` reconcile.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EgressApprovalStatus {
    /// One of `Pending` (waiting for sibling sandbox or admission
    /// failure recoverable), `Active` (mount in place + router echoed
    /// the merged digest), `Expired` (TTL elapsed, mount removed,
    /// baseline re-echoed). `None` when the CR was just admitted and
    /// the reconciler hasn't run yet.
    #[serde(default)]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default)]
    pub observed_generation: Option<i64>,

    /// RFC 3339 timestamp at which the approval first became active.
    /// Set on the `Pending → Active` transition and never changes
    /// afterwards (the TTL is measured from this point; re-reconciles
    /// MUST NOT bump it).
    #[serde(default)]
    pub effective_at: Option<String>,

    /// RFC 3339 timestamp at which the approval expires. Computed as
    /// `effective_at + spec.ttl` at the `Pending → Active`
    /// transition. Stable across re-reconciles.
    #[serde(default)]
    pub expires_at: Option<String>,

    /// `sha256:<hex>` digest of the canonical merged
    /// (baseline ∪ approval.hosts) allowlist this approval contributes
    /// to. Identical to the value the router echoes via
    /// `GET /internal/policy-status` once the mount lands.
    #[serde(default)]
    pub merged_digest: Option<String>,

    /// Number of hosts in `spec.hosts`. Mirrored to status for kubectl
    /// printer-column convenience; the CRD-derive printcolumn config
    /// expects this field by name.
    #[serde(default)]
    pub host_count: Option<i64>,

    /// Number of requests the router has allowed via **this**
    /// approval (i.e. a hostname that matched an approval entry and
    /// would NOT have matched the baseline alone). Reported by the
    /// router and pushed via `/internal/policy-status`. Useful for
    /// auditors evaluating whether an approval was actually needed.
    #[serde(default)]
    pub usage_count: Option<i64>,

    /// Standard k8s conditions. Reasons drawn from
    /// [`condition_reasons`].
    #[serde(default)]
    pub conditions: Option<Vec<Condition>>,
}

/// Canonical condition reasons emitted on `EgressApproval` status.
///
/// Stable strings — public CLIs and dashboards key off them. Adding
/// new reasons is fine; renaming an existing one is a breaking change
/// (treat as CRD migration).
pub mod condition_reasons {
    /// The sibling `ClawSandbox` named by `spec.sandbox` does not
    /// exist or is not yet `phase=Ready`. The approval stays in
    /// `Pending`; the reconciler retries.
    pub const BLOCKED_ON_SANDBOX: &str = "BlockedOnSandbox";

    /// The mount file landed and the router echoed the merged
    /// digest. The approval is `Active`.
    pub const ROUTER_CONFIRMED: &str = "RouterConfirmed";

    /// The router has not yet echoed the merged digest. Transient.
    pub const AWAITING_ROUTER_ECHO: &str = "AwaitingRouterEcho";

    /// `spec.ttl` exceeds the cluster ceiling (`maxApprovalTtl`).
    /// The approval is rejected (Pending with a terminal reason);
    /// admission CEL should also catch this, but the reconciler
    /// emits this reason in case the ceiling changed under us.
    pub const TTL_EXCEEDS_CEILING: &str = "TtlExceedsCeiling";

    /// `spec.ttl` failed to parse as a positive ISO 8601 duration.
    /// CEL should catch this; defense-in-depth reason.
    pub const TTL_INVALID: &str = "TtlInvalid";

    /// `spec.reason` contained ASCII control bytes (excluding tab /
    /// newline / carriage return). Rejected as malicious audit-log
    /// injection input.
    pub const REASON_INVALID: &str = "ReasonInvalid";

    /// The TTL elapsed; the mount is removed and the router echoed
    /// the post-removal merged (= baseline) digest.
    pub const EXPIRED: &str = "Expired";
}

impl EgressApproval {
    /// Convenience: hostname canonicalization used by canonical-form
    /// merge + audit log. Mirrors
    /// `policy_canonical::egress::CanonicalEndpoint` shape.
    pub fn merged_host_count(&self) -> usize {
        self.spec.hosts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    fn sample_spec() -> EgressApprovalSpec {
        EgressApprovalSpec {
            sandbox: "demo".to_string(),
            hosts: vec![
                EndpointConfig {
                    host: "example.com".to_string(),
                    port: Some(443),
                },
                EndpointConfig {
                    host: "api.example.com".to_string(),
                    port: None,
                },
            ],
            reason: "INC-1234 incident response".to_string(),
            ticket: Some("INC-1234".to_string()),
            ttl: "PT15M".to_string(),
        }
    }

    #[test]
    fn spec_round_trips_with_all_fields() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serializes");
        let back: EgressApprovalSpec = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.sandbox, "demo");
        assert_eq!(back.hosts.len(), 2);
        assert_eq!(back.reason, "INC-1234 incident response");
        assert_eq!(back.ticket.as_deref(), Some("INC-1234"));
        assert_eq!(back.ttl, "PT15M");
    }

    #[test]
    fn spec_round_trips_with_ticket_omitted() {
        let mut spec = sample_spec();
        spec.ticket = None;
        let json = serde_json::to_string(&spec).expect("serializes");
        // `ticket` is skip_serializing_if = Option::is_none — must NOT appear.
        assert!(
            !json.contains("ticket"),
            "ticket=None must be omitted from wire form; got {json}"
        );
        let back: EgressApprovalSpec = serde_json::from_str(&json).expect("deserializes");
        assert!(back.ticket.is_none());
    }

    #[test]
    fn spec_uses_camel_case_on_the_wire() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serializes");
        assert!(json.contains("\"sandbox\""));
        assert!(json.contains("\"hosts\""));
        assert!(json.contains("\"reason\""));
        assert!(json.contains("\"ttl\""));
        // Nothing snake_case in the spec — bare nouns, no camelization needed.
    }

    #[test]
    fn status_round_trips_with_all_fields() {
        let status = EgressApprovalStatus {
            phase: Some("Active".to_string()),
            observed_generation: Some(3),
            effective_at: Some("2026-05-15T10:00:00Z".to_string()),
            expires_at: Some("2026-05-15T10:15:00Z".to_string()),
            merged_digest: Some("sha256:abc123".to_string()),
            host_count: Some(2),
            usage_count: Some(7),
            conditions: Some(vec![]),
        };
        let json = serde_json::to_string(&status).expect("serializes");
        // camelCase keys per #[serde(rename_all = "camelCase")].
        assert!(json.contains("\"observedGeneration\""));
        assert!(json.contains("\"effectiveAt\""));
        assert!(json.contains("\"expiresAt\""));
        assert!(json.contains("\"mergedDigest\""));
        assert!(json.contains("\"hostCount\""));
        assert!(json.contains("\"usageCount\""));
        let back: EgressApprovalStatus = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.phase.as_deref(), Some("Active"));
        assert_eq!(back.observed_generation, Some(3));
        assert_eq!(back.host_count, Some(2));
    }

    #[test]
    fn status_defaults_to_all_none() {
        let s = EgressApprovalStatus::default();
        assert!(s.phase.is_none());
        assert!(s.observed_generation.is_none());
        assert!(s.effective_at.is_none());
        assert!(s.expires_at.is_none());
        assert!(s.merged_digest.is_none());
        assert!(s.host_count.is_none());
        assert!(s.usage_count.is_none());
        assert!(s.conditions.is_none());
    }

    #[test]
    fn merged_host_count_matches_spec_hosts_len() {
        let approval = EgressApproval::new("demo-approval", sample_spec());
        assert_eq!(approval.merged_host_count(), 2);
        let approval = EgressApproval::new(
            "empty",
            EgressApprovalSpec {
                hosts: vec![],
                ..sample_spec()
            },
        );
        assert_eq!(approval.merged_host_count(), 0);
    }

    #[test]
    fn condition_reasons_are_stable_strings() {
        // Wire-contract pin — these strings appear in CLI output,
        // dashboards, and operator runbooks. Renames are CRD-migration
        // events. This test fails loudly on any change.
        use condition_reasons::*;
        assert_eq!(BLOCKED_ON_SANDBOX, "BlockedOnSandbox");
        assert_eq!(ROUTER_CONFIRMED, "RouterConfirmed");
        assert_eq!(AWAITING_ROUTER_ECHO, "AwaitingRouterEcho");
        assert_eq!(TTL_EXCEEDS_CEILING, "TtlExceedsCeiling");
        assert_eq!(TTL_INVALID, "TtlInvalid");
        assert_eq!(REASON_INVALID, "ReasonInvalid");
        assert_eq!(EXPIRED, "Expired");
    }

    #[test]
    fn crd_shape_pinned() {
        let crd = EgressApproval::crd();
        assert_eq!(crd.spec.group, "azureclaw.azure.com");
        assert_eq!(crd.spec.names.kind, "EgressApproval");
        assert_eq!(crd.spec.names.plural, "egressapprovals");
        assert_eq!(
            crd.spec.names.short_names.as_deref(),
            Some(&["eappr".to_string()][..])
        );
        assert_eq!(crd.spec.scope, "Namespaced");
        assert_eq!(crd.spec.versions.len(), 1);
        assert_eq!(crd.spec.versions[0].name, "v1alpha1");
        assert!(crd.spec.versions[0].served);
        assert!(crd.spec.versions[0].storage);
        // status subresource enabled
        assert!(crd.spec.versions[0].subresources.is_some());
    }

    #[test]
    fn crd_advertises_status_printcolumns() {
        let crd = EgressApproval::crd();
        let cols = crd.spec.versions[0]
            .additional_printer_columns
            .as_ref()
            .expect("printcolumns present");
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Sandbox"));
        assert!(names.contains(&"Phase"));
        assert!(names.contains(&"Expires"));
        assert!(names.contains(&"Hosts"));
        assert!(names.contains(&"Age"));
    }
}
