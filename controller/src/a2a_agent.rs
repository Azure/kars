// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `A2AAgent` CRD — full reconciler (Phase 2 §8 entry 4 / S3).
//!
//! Status: **full-schema** as of `phase2/a2aagent-reconciler` (S3 of
//! Phase 2). The reconciler in `controller/src/a2a_agent_reconciler.rs`
//! compiles each CR to an A2A 1.2 **AgentCard** JSON and publishes it
//! as a `ConfigMap` (`a2aagent-{name}-card`). The router-side
//! `/.well-known/agent.json` mount that serves the card and the
//! corresponding signing-key Secret are wired in S7
//! (`phase2-conditions-ssa-leader`, the informer pass).
//!
//! ## Spec sources
//!
//! - A2A 1.2 AgentCard: <https://a2a-protocol.org/spec/v1.2/agent-card>
//! - AP2 commerce policy reference (linked via `spec.policyRefs.toolPolicy`):
//!   <https://ap2-protocol.org/>
//! - JWS detached signature (RFC 7515 §A.5) for AgentCard signing
//!   (router-side, S7).
//!
//! ## Why this scaffold lands now
//!
//! - Phase 1 already shipped the **router-side projection** —
//!   `inference-router/src/a2a/agent_projection.rs::A2aAgentSpec` is the
//!   pure-function consumer of this CRD's projected shape. S3 closes
//!   the operator side (`kubectl apply` → cluster artifact).
//! - Phase 2 §14.6 column 4 (A2A 1.2 + AP2) is conditional on this CRD
//!   reaching `Ready`.
//!
//! All field names track the A2A 1.2 AgentCard spec exactly so that the
//! ConfigMap body is the wire-format card with no per-router
//! transformations.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `A2AAgent.spec` — declares an A2A 1.2 agent reachable at
/// `endpointUrl` and the trust anchors peers must use to authenticate
/// inbound signatures.
///
/// One CR yields one cluster-resident AgentCard ConfigMap. The router
/// (S7) projects the union of all `A2AAgent` CRs into its trust store
/// via `inference-router::a2a::snapshot_rebuild::rebuild_snapshot`.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "A2AAgent",
    namespaced,
    status = "A2AAgentStatus",
    shortname = "a2a",
    printcolumn = r#"{"name":"Endpoint","type":"string","jsonPath":".spec.endpointUrl"}"#,
    printcolumn = r#"{"name":"Production","type":"boolean","jsonPath":".spec.productionMode"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct A2AAgentSpec {
    /// HTTPS URL where the agent serves `/.well-known/agent.json` and
    /// the JSON-RPC `message/send` / `tasks/get` / `tasks/cancel`
    /// endpoints. Validated by admission CEL: must be `https://` when
    /// `productionMode == true`.
    pub endpoint_url: String,

    /// One or more Ed25519 signing keys peer agents use to verify
    /// AgentCard JWS detached signatures and inbound JSON-RPC envelope
    /// signatures (A2A 1.2 §6.3). Must contain at least one entry —
    /// CEL-enforced.
    ///
    /// **Operator-supplied.** Auto-generation of a controller-managed
    /// keypair is an S7 follow-up (router-side mount required); this
    /// slice deliberately does not write any Secret.
    pub signing_keys: Vec<A2aSigningKey>,

    /// Production-mode flag. Mirrors `McpServer.spec.productionMode` —
    /// when `true`, the router will reject unauthenticated traffic, and
    /// admission CEL enforces `endpointUrl.startsWith("https://")` plus
    /// non-empty `signingKeys`.
    #[serde(default)]
    pub production_mode: bool,

    /// Declared A2A protocol capabilities advertised in the AgentCard
    /// (`tasks`, `streaming`, `cancel`, `mandates`, etc.). Free-form
    /// string list — the spec's vocabulary evolves; keeping it stringly
    /// typed matches the spec's open registry approach.
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Trust thresholds applied by the router-side verifier (S7) when
    /// processing inbound signed envelopes from peers. None ⇒ fall back
    /// to the cluster-default profile.
    pub trust: Option<TrustThresholds>,

    /// Federation — peer A2A agents this agent acknowledges as part of
    /// its trust circle. Renders into the AgentCard's
    /// `federation.peers[]` array. Cross-CR references (resolution and
    /// validity checks) happen in S7.
    #[serde(default)]
    pub federation: Vec<FederationPeer>,

    /// Soft references to other CRDs that contribute policy. Not
    /// resolved here — the router (S7) joins the AgentCard's
    /// declarations with the referenced `ToolPolicy` to enforce AP2
    /// `commerce` / `approval` / `rateLimit` at request time.
    pub policy_refs: Option<PolicyRefs>,

    /// Optional human-readable name shown in operator TUI and embedded
    /// in the AgentCard `name` field.
    pub display_name: Option<String>,

    /// Optional human-readable description embedded in the AgentCard
    /// `description` field. AgentCard spec §3.1.
    pub description: Option<String>,
}

/// One signing key entry; mirrors
/// `inference-router::a2a::agent_projection::A2aAgentSigningKeySpec`
/// 1:1 — same `kid` / `alg` / `publicKeyB64u` / `notAfter`. Reusing
/// the projected shape verbatim avoids a translation layer between
/// controller → ConfigMap → router projection.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct A2aSigningKey {
    /// Stable identifier exposed to verifying peers. Must be non-empty
    /// and unique across this CR's `signingKeys[]` (CEL-enforced).
    pub kid: String,

    /// Algorithm pin. Currently only `"EdDSA"` is honoured; the
    /// router-side projection rejects any other value at runtime.
    pub alg: String,

    /// Ed25519 public key, base64url-encoded with NO padding (RFC
    /// 7515 §2). Decoded length is asserted to be 32 bytes by the
    /// router-side projection.
    pub public_key_b64u: String,

    /// Optional Unix-seconds expiry. `None` ⇒ never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
}

/// Trust thresholds applied at the router when processing inbound
/// signed envelopes from peers. All fields optional with sensible
/// defaults documented in `docs/security.md`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrustThresholds {
    /// When `true`, the router rejects any inbound A2A request that
    /// lacks a valid JWS detached signature from a known peer key.
    #[serde(default)]
    pub require_signed_requests: bool,

    /// Minimum number of independent valid signatures required (A2A
    /// 1.2 §6.4 multi-sig flow). Default 1; a value > 1 makes sense
    /// only when the inbound envelope is co-signed (e.g., AP2 cart
    /// mandates).
    #[serde(default)]
    pub min_signatures_required: Option<u32>,

    /// Maximum tolerated clock skew (seconds) between the signing
    /// peer and this verifier. Default 60s; ranges below 5s make
    /// the verifier brittle to NTP wobble.
    #[serde(default)]
    pub max_clock_skew_seconds: Option<i64>,
}

/// One federation peer entry. Same-cluster peers reference an
/// `A2AAgent` CR by `name`; cross-cluster peers carry the full
/// `endpointUrl` + key pin so the trust decision is local.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FederationPeer {
    /// Short label rendered in audit logs and the AgentCard
    /// `federation.peers[*].label`.
    pub label: String,

    /// Either `"in-cluster"` (then `agentRef` is consulted) or
    /// `"external"` (then `endpointUrl` + `pinnedKid` are consulted).
    /// CEL ensures exactly one of `agentRef` / `endpointUrl` is set.
    pub kind: String,

    /// In-cluster peer: name of the `A2AAgent` CR in the same
    /// namespace. Cross-namespace federation is deliberately not
    /// allowed — operators wire that explicitly through `external`
    /// peers with full URL + key pin.
    pub agent_ref: Option<String>,

    /// External peer: `https://` URL where the peer agent serves
    /// `/.well-known/agent.json`.
    pub endpoint_url: Option<String>,

    /// External peer: pinned `kid` the local verifier expects on the
    /// peer's outbound signatures. Defends against peer-side key
    /// rotation breaking the federation silently.
    pub pinned_kid: Option<String>,
}

/// References to other CRDs that contribute policy. Names only — no
/// namespace plumbing because all referenced CRs must be in the same
/// namespace as the `A2AAgent`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRefs {
    /// Name of a `ToolPolicy` CR whose `commerce` / `approval` /
    /// `rateLimit` blocks apply to A2A `message/send` requests on this
    /// agent. The router (S7) joins both CRs at request time.
    pub tool_policy: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct A2AAgentStatus {
    /// Lifecycle phase: `Pending` | `Ready` | `Degraded` | `Unknown`.
    #[serde(default)]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default)]
    pub observed_generation: Option<i64>,

    /// Standard K8s Conditions (KEP-1623 shape). Populated by
    /// `controller/src/status/conditions.rs::preserve_transition_time`
    /// — the same helper used by S1 (MCP) and S2 (ToolPolicy).
    #[serde(default)]
    pub conditions: Option<Vec<Condition>>,

    /// Reference to the published AgentCard ConfigMap
    /// (`a2aagent-{name}-card`, key `agent.json`). Field-managed by
    /// `azureclaw-controller/a2aagent`. Reuses the
    /// [`crate::mcp_server::LocalObjectRef`] shape to avoid a
    /// duplicate type — single struct, two semantic names per the
    /// no-duplication rule.
    #[serde(default)]
    pub agent_card_config_map_ref: Option<crate::mcp_server::LocalObjectRef>,

    /// SHA-256 prefix (32 hex chars) of the canonicalised compiled
    /// AgentCard. Same shape as `ToolPolicy.status.versionHash` —
    /// router-side hot-reload short-circuits redundant snapshot
    /// rebuilds when this hasn't moved.
    #[serde(default)]
    pub version_hash: Option<String>,

    /// RFC-3339 timestamp of the last successful compile.
    #[serde(default)]
    pub last_compiled_at: Option<String>,
}
