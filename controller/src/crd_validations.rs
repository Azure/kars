// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CEL admission validations for AzureClaw CRDs.
//!
//! Phase 1 deliverable §7 entry 12: every new CRD must ship with
//! `x-kubernetes-validations` (CEL) rules that catch malformed specs at
//! `kubectl apply` time, before the reconciler ever sees them.
//!
//! ## Why CEL, not the reconciler
//!
//! - **Defence in depth.** Admission rejects bad CRs before they hit
//!   etcd; the reconciler is a second line of defence, not the only
//!   one.
//! - **No reconciler races.** A CR with `productionMode: true` and
//!   missing `oauth` would otherwise sit in `Pending` forever while
//!   operators wonder why; CEL turns it into an immediate `kubectl
//!   apply` error.
//! - **Clear blast radius.** A buggy reconciler can be patched and
//!   rolled out; a malformed CR that bypassed CEL has already
//!   committed to etcd and may be referenced by other resources.
//!
//! ## How rules are wired
//!
//! kube-rs's `CustomResource` derive generates the JSON-schema
//! envelope but does **not** emit `x-kubernetes-validations` from
//! `#[kube(...)]` attributes (no upstream support yet — kube-rs#1557).
//! Phase 1 plumbs the rules in by post-processing the generated
//! `CustomResourceDefinition`:
//!
//! 1. Call `<Type>::crd()` to materialise the schema-only CRD.
//! 2. Call [`mcp_server_crd`] / [`tool_policy_crd`] in this module
//!    to walk the schema tree and inject the rules.
//! 3. Hand the result to the apply pipeline (`kubectl apply -f -` or
//!    `kube::Api::patch`).
//!
//! ## Coverage
//!
//! Each `*_validations` constant function returns the exact rule list
//! that the corresponding CRD ships with. Tests assert:
//!
//! - The list is non-empty (no CRD ships with zero CEL coverage).
//! - Every rule has both `rule` and `message` populated (operator UX).
//! - The injection round-trips through the `JSONSchemaProps` tree.

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, ValidationRule,
};
use kube::CustomResourceExt;

use crate::a2a_agent::A2AAgent;
use crate::claw_eval::ClawEval;
use crate::claw_memory::ClawMemory;
use crate::inference_policy::InferencePolicy;
use crate::mcp_server::McpServer;
use crate::tool_policy::ToolPolicy;

/// `McpServer.spec` CEL rules.
///
/// Returns the validations injected on the `spec` schema node:
///
/// 1. `productionMode == true` requires `oauth.issuer` to be set.
/// 2. `productionMode == true` requires the URL to start with
///    `https://`.
/// 3. `oauth.pkce`, when present, must be `S256` (RFC 7636 §4.2 — the
///    one PKCE method this CRD supports).
#[must_use]
pub fn mcp_server_validations() -> Vec<ValidationRule> {
    vec![
        ValidationRule {
            rule:
                "self.productionMode == false || (has(self.oauth) && size(self.oauth.issuer) > 0)"
                    .into(),
            message: Some("productionMode requires spec.oauth.issuer to be set".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "self.productionMode == false || self.url.startsWith('https://')".into(),
            message: Some("productionMode requires spec.url to begin with https://".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.oauth) || !has(self.oauth.pkce) || self.oauth.pkce == 'S256'".into(),
            message: Some("spec.oauth.pkce, when set, must be 'S256' (RFC 7636 §4.2)".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
}

/// `ToolPolicy.spec` CEL rules.
///
/// Note: `commerce.{dailyCap,monthlyCap}` are formatted currency
/// strings (e.g. `"USD 100.00"`), not integers — the original
/// numeric comparison rules were ill-typed and the K8s 1.31+ CEL
/// compiler rejects them outright. Range/ordering validation
/// happens in the policy compiler at admission time
/// (`policy_compiler::parse_currency`); the CRD-side guard is
/// limited to *structural* checks expressible in CEL.
///
/// 1. `appliesTo.sandboxMatchLabels` non-empty (a policy that matches
///    nothing is almost always an authoring mistake; if truly
///    intended, an explicit `disabled: true` field is the right
///    escape hatch — not silently empty selectors).
#[must_use]
pub fn tool_policy_validations() -> Vec<ValidationRule> {
    vec![ValidationRule {
        rule:
            "has(self.appliesTo.sandboxMatchLabels) && size(self.appliesTo.sandboxMatchLabels) > 0"
                .into(),
        message: Some("spec.appliesTo.sandboxMatchLabels must contain at least one label".into()),
        reason: Some("FieldValueInvalid".into()),
        ..ValidationRule::default()
    }]
}

/// Inject CEL rules onto the `spec` schema node of a generated CRD.
///
/// Returns `None` when the schema tree is shaped unexpectedly (no
/// versions, no schema, no spec property). Callers should treat
/// `None` as a hard programmer error and panic — the input is always
/// produced by kube-rs's derive, so a `None` here is a build-time
/// regression, not a runtime concern.
fn inject_spec_validations(
    mut crd: CustomResourceDefinition,
    rules: Vec<ValidationRule>,
) -> Option<CustomResourceDefinition> {
    let version = crd.spec.versions.first_mut()?;
    let schema = version.schema.as_mut()?;
    let root = schema.open_api_v3_schema.as_mut()?;
    let props = root.properties.as_mut()?;
    let spec = props.get_mut("spec")?;
    spec.x_kubernetes_validations = Some(rules);
    Some(crd)
}

/// `McpServer` CRD with [`mcp_server_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing
/// — see [`inject_spec_validations`].
#[must_use]
pub fn mcp_server_crd() -> CustomResourceDefinition {
    inject_spec_validations(McpServer::crd(), mcp_server_validations())
        .expect("kube-rs derive must produce a spec property on McpServer")
}

/// `ToolPolicy` CRD with [`tool_policy_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing.
#[must_use]
pub fn tool_policy_crd() -> CustomResourceDefinition {
    inject_spec_validations(ToolPolicy::crd(), tool_policy_validations())
        .expect("kube-rs derive must produce a spec property on ToolPolicy")
}

/// `A2AAgent.spec` CEL rules.
///
/// 1. `signingKeys` must be non-empty (an A2A agent without a signing
///    key cannot be authenticated by peers — the authoring intent of
///    such a spec is almost always a mistake).
/// 2. Every `signingKeys[*].alg` must be `"EdDSA"` — the only
///    algorithm the router-side projection
///    (`inference-router::a2a::agent_projection`) honours.
/// 3. `productionMode == true` requires `endpointUrl` to begin with
///    `https://` (mirrors the `McpServer` rule).
/// 4. Federation peers must be either in-cluster (`kind == "in-cluster"`,
///    `agentRef` set) or external (`kind == "external"`,
///    `endpointUrl` + `pinnedKid` set) — not both, not neither.
#[must_use]
pub fn a2a_agent_validations() -> Vec<ValidationRule> {
    vec![
        ValidationRule {
            rule: "size(self.signingKeys) > 0".into(),
            message: Some("spec.signingKeys must contain at least one entry".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "self.signingKeys.all(k, k.alg == 'EdDSA')".into(),
            message: Some("spec.signingKeys[*].alg must be 'EdDSA' (only supported algorithm)".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "self.productionMode == false || self.endpointUrl.startsWith('https://')".into(),
            message: Some(
                "productionMode requires spec.endpointUrl to begin with https://".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "self.federation.all(p, (p.kind == 'in-cluster' && has(p.agentRef) && !has(p.endpointUrl)) || (p.kind == 'external' && has(p.endpointUrl) && has(p.pinnedKid) && !has(p.agentRef)))".into(),
            message: Some(
                "federation peers: kind 'in-cluster' requires agentRef only; kind 'external' requires endpointUrl + pinnedKid".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
}

/// `A2AAgent` CRD with [`a2a_agent_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing.
#[must_use]
pub fn a2a_agent_crd() -> CustomResourceDefinition {
    inject_spec_validations(A2AAgent::crd(), a2a_agent_validations())
        .expect("kube-rs derive must produce a spec property on A2AAgent")
}

/// `InferencePolicy.spec` CEL rules. Phase 2 §8 entry 4 (S4).
///
/// Returns the validations injected on the `spec` schema node:
///
/// - `tokenBudget.monthlyTokens >= tokenBudget.dailyTokens` when both
///   are set (admission CEL — the runtime path also checks but
///   prevents the half-baked CR from ever landing).
/// - `tokenBudget.monthlyTokens >= tokenBudget.perRequestTokens` when
///   both are set (single request can't blow a monthly budget that
///   wouldn't accept it).
/// - `contentSafety.{hate,selfHarm,sexual,violence}` ∈ {`Safe`, `Low`,
///   `Medium`, `High`} when present — matches Microsoft Content Safety
///   `Microsoft.DefaultV2` severity levels exactly.
/// - `modelPreference.primary` requires non-empty `provider` and
///   `deployment` (any fallback entry too).
/// - `appliesTo.action` ∈ {`chat`, `responses`, `image`, `embeddings`,
///   `*`} — closed set matching `inference-router/src/routes/inference.rs`
///   call-site enumeration.
#[must_use]
pub fn inference_policy_validations() -> Vec<ValidationRule> {
    let severities = "['Safe','Low','Medium','High']";
    vec![
        ValidationRule {
            rule: "!has(self.tokenBudget) || !has(self.tokenBudget.monthlyTokens) || !has(self.tokenBudget.dailyTokens) || self.tokenBudget.monthlyTokens >= self.tokenBudget.dailyTokens".into(),
            message: Some("spec.tokenBudget.monthlyTokens must be >= spec.tokenBudget.dailyTokens".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.tokenBudget) || !has(self.tokenBudget.monthlyTokens) || !has(self.tokenBudget.perRequestTokens) || self.tokenBudget.monthlyTokens >= self.tokenBudget.perRequestTokens".into(),
            message: Some("spec.tokenBudget.monthlyTokens must be >= spec.tokenBudget.perRequestTokens".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: format!(
                "!has(self.contentSafety) || (\
                 (!has(self.contentSafety.hate)      || self.contentSafety.hate      in {sev}) && \
                 (!has(self.contentSafety.selfHarm)  || self.contentSafety.selfHarm  in {sev}) && \
                 (!has(self.contentSafety.sexual)    || self.contentSafety.sexual    in {sev}) && \
                 (!has(self.contentSafety.violence)  || self.contentSafety.violence  in {sev}))",
                sev = severities
            ),
            message: Some("spec.contentSafety severities must be one of: Safe, Low, Medium, High".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.modelPreference) || (size(self.modelPreference.primary.provider) > 0 && size(self.modelPreference.primary.deployment) > 0)".into(),
            message: Some("spec.modelPreference.primary requires non-empty provider and deployment".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.modelPreference) || self.modelPreference.fallback.all(f, size(f.provider) > 0 && size(f.deployment) > 0)".into(),
            message: Some("spec.modelPreference.fallback[*] requires non-empty provider and deployment".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.appliesTo.action) || self.appliesTo.action in ['chat','responses','image','embeddings','*']".into(),
            message: Some("spec.appliesTo.action must be one of: chat, responses, image, embeddings, *".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
}

/// `InferencePolicy` CRD with [`inference_policy_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing.
#[must_use]
pub fn inference_policy_crd() -> CustomResourceDefinition {
    inject_spec_validations(InferencePolicy::crd(), inference_policy_validations())
        .expect("kube-rs derive must produce a spec property on InferencePolicy")
}

/// `ClawMemory.spec` CEL rules. Phase 2 §8 entry 5 (S5).
///
/// `ClawMemory` is a binding/provisioning resource over Azure AI
/// Foundry Memory Store (per `docs/implementation-plan.md` §3
/// non-compete). The CRD is shape-only — runtime auth and Foundry
/// availability are out of admission scope. Rules:
///
/// - `storeName` non-empty + DNS-label-style (lowercase alnum + `-`,
///   1-63 chars). Foundry treats store names as case-sensitive
///   identifiers; pinning to DNS-label form keeps them safe to use
///   verbatim in URLs and ConfigMap labels.
/// - `sandboxRef.name` non-empty (1-253 chars; same length cap as
///   K8s object names).
/// - `scope` non-empty (1-256 chars). Foundry uses scope as a
///   partition key — empty scope would cross-contaminate every
///   sandbox bound to the same store.
/// - `retentionDays > 0` when present. Zero would request immediate
///   deletion, which is what `delete_scope` is for.
#[must_use]
pub fn claw_memory_validations() -> Vec<ValidationRule> {
    vec![
        ValidationRule {
            rule: "size(self.storeName) > 0 && size(self.storeName) <= 63 && self.storeName.matches('^[a-z0-9]([-a-z0-9]*[a-z0-9])?$')".into(),
            message: Some(
                "spec.storeName must be a DNS-label (1-63 chars, lowercase alphanumeric + dashes)".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "size(self.sandboxRef.name) > 0 && size(self.sandboxRef.name) <= 253".into(),
            message: Some("spec.sandboxRef.name must be 1-253 characters".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "size(self.scope) > 0 && size(self.scope) <= 256".into(),
            message: Some("spec.scope must be 1-256 characters".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.retentionDays) || self.retentionDays > 0".into(),
            message: Some(
                "spec.retentionDays must be > 0 when set (use delete_scope for immediate deletion)".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
}

/// `ClawMemory` CRD with [`claw_memory_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing.
#[must_use]
pub fn claw_memory_crd() -> CustomResourceDefinition {
    inject_spec_validations(ClawMemory::crd(), claw_memory_validations())
        .expect("kube-rs derive must produce a spec property on ClawMemory")
}

/// `ClawEval.spec` CEL rules. Phase 2 §8 entry 6 (S6).
///
/// `ClawEval` is a binding/provisioning resource over Azure AI
/// Foundry Evals (per `docs/implementation-plan.md` §10.5 #6). The CRD
/// is shape-only — Foundry availability and runtime trigger semantics
/// are out of admission scope. Rules:
///
/// - `sandboxRef.name` non-empty (1-253 chars; same length cap as
///   K8s object names).
/// - `evaluators`: each entry 1-256 chars; required (`size >= 1`)
///   when `suite == "foundry-evals"`. Other suites accept empty.
/// - `schedule`, when set, looks like a 5-or-6-token cron line. We do
///   not parse cron at admission (defer to runtime), but we reject
///   empty strings and impossible token counts.
/// - `threshold.score`, when set, in `[0.0, 1.0]`.
/// - `dataset`: at most one of `configMapRef` / `inline` (mutually
///   exclusive). `inline` capped at 64 entries to keep the CR small.
/// - `displayName`, when set, 1-256 chars.
#[must_use]
pub fn claw_eval_validations() -> Vec<ValidationRule> {
    vec![
        ValidationRule {
            rule: "size(self.sandboxRef.name) > 0 && size(self.sandboxRef.name) <= 253".into(),
            message: Some("spec.sandboxRef.name must be 1-253 characters".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "self.suite != 'foundry-evals' || (has(self.evaluators) && size(self.evaluators) >= 1)".into(),
            message: Some(
                "spec.evaluators must contain at least one entry when spec.suite is 'foundry-evals'".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.evaluators) || self.evaluators.all(e, size(e) > 0 && size(e) <= 256)".into(),
            message: Some("each spec.evaluators entry must be 1-256 characters".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.schedule) || (size(self.schedule) > 0 && size(self.schedule) <= 256 && (size(self.schedule.split(' ')) == 5 || size(self.schedule.split(' ')) == 6))".into(),
            message: Some(
                "spec.schedule, when set, must be a 5-or-6-field cron expression (1-256 chars)".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.threshold) || (self.threshold.score >= 0.0 && self.threshold.score <= 1.0)".into(),
            message: Some("spec.threshold.score must be in [0.0, 1.0] when set".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.dataset) || !(has(self.dataset.configMapRef) && has(self.dataset.inline) && size(self.dataset.inline) > 0)".into(),
            message: Some(
                "spec.dataset.configMapRef and spec.dataset.inline are mutually exclusive".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.dataset) || !has(self.dataset.inline) || size(self.dataset.inline) <= 64".into(),
            message: Some(
                "spec.dataset.inline is capped at 64 entries; use a ConfigMap for larger datasets".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.displayName) || (size(self.displayName) > 0 && size(self.displayName) <= 256)".into(),
            message: Some("spec.displayName, when set, must be 1-256 characters".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
}

/// `ClawEval` CRD with [`claw_eval_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing.
#[must_use]
pub fn claw_eval_crd() -> CustomResourceDefinition {
    inject_spec_validations(ClawEval::crd(), claw_eval_validations())
        .expect("kube-rs derive must produce a spec property on ClawEval")
}

/// `TrustGraph.spec` CEL rules. Phase F1.
///
/// 1. `vertices` must be non-empty (an empty graph yields a useless
///    projection — the operator likely meant to delete the CR).
/// 2. Every `vertices[*].alg` must be `"EdDSA"` — only supported.
/// 3. Every `edges[*].score` must be in `[0, 1000]` (the AGT
///    trust-score domain — same range as
///    `ClawSandbox.spec.governance.trustThreshold`).
/// 4. Every `edges[*].notAfter`, when set, must be `>= issuedAt`
///    (an inverted-expiry edge cannot represent a meaningful
///    attestation lifetime).
#[must_use]
pub fn trust_graph_validations() -> Vec<ValidationRule> {
    vec![
        ValidationRule {
            rule: "size(self.vertices) > 0".into(),
            message: Some("spec.vertices must contain at least one entry".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "self.vertices.all(v, v.alg == 'EdDSA')".into(),
            message: Some(
                "spec.vertices[*].alg must be 'EdDSA' (only supported algorithm)".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.edges) || self.edges.all(e, e.score >= 0 && e.score <= 1000)"
                .into(),
            message: Some("spec.edges[*].score must be in [0, 1000]".into()),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.edges) || self.edges.all(e, !has(e.notAfter) || e.notAfter >= e.issuedAt)".into(),
            message: Some(
                "spec.edges[*].notAfter, when set, must be >= issuedAt".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
}

/// `TrustGraph` CRD with [`trust_graph_validations`] injected.
///
/// Panics only if kube-rs ever produces a CRD whose `spec` is missing.
#[must_use]
pub fn trust_graph_crd() -> CustomResourceDefinition {
    inject_spec_validations(
        crate::trust_graph::TrustGraph::crd(),
        trust_graph_validations(),
    )
    .expect("kube-rs derive must produce a spec property on TrustGraph")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_validations(crd: &CustomResourceDefinition) -> &Vec<ValidationRule> {
        crd.spec
            .versions
            .first()
            .and_then(|v| v.schema.as_ref())
            .and_then(|s| s.open_api_v3_schema.as_ref())
            .and_then(|r| r.properties.as_ref())
            .and_then(|p| p.get("spec"))
            .and_then(|s| s.x_kubernetes_validations.as_ref())
            .expect("spec.x-kubernetes-validations must be present after injection")
    }

    #[test]
    fn mcp_server_validations_are_non_empty() {
        assert!(!mcp_server_validations().is_empty());
    }

    #[test]
    fn tool_policy_validations_are_non_empty() {
        assert!(!tool_policy_validations().is_empty());
    }

    #[test]
    fn every_mcp_server_rule_has_message_and_rule() {
        for rule in mcp_server_validations() {
            assert!(!rule.rule.is_empty(), "rule body must not be empty");
            let msg = rule.message.as_deref().unwrap_or("");
            assert!(!msg.is_empty(), "rule '{}' missing message", rule.rule);
        }
    }

    #[test]
    fn every_tool_policy_rule_has_message_and_rule() {
        for rule in tool_policy_validations() {
            assert!(!rule.rule.is_empty(), "rule body must not be empty");
            let msg = rule.message.as_deref().unwrap_or("");
            assert!(!msg.is_empty(), "rule '{}' missing message", rule.rule);
        }
    }

    #[test]
    fn mcp_server_crd_has_spec_validations_after_injection() {
        let crd = mcp_server_crd();
        let v = spec_validations(&crd);
        assert_eq!(v.len(), mcp_server_validations().len());
    }

    #[test]
    fn tool_policy_crd_has_spec_validations_after_injection() {
        let crd = tool_policy_crd();
        let v = spec_validations(&crd);
        assert_eq!(v.len(), tool_policy_validations().len());
    }

    #[test]
    fn mcp_server_rules_mention_production_mode_oauth_invariant() {
        let rules: Vec<String> = mcp_server_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules
                .iter()
                .any(|r| r.contains("productionMode") && r.contains("oauth")),
            "must enforce productionMode -> oauth invariant; got rules: {rules:?}"
        );
    }

    #[test]
    fn tool_policy_rules_enforce_non_empty_match_labels() {
        let rules: Vec<String> = tool_policy_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules
                .iter()
                .any(|r| r.contains("sandboxMatchLabels") && r.contains("size(")),
            "must require non-empty appliesTo.sandboxMatchLabels; got rules: {rules:?}"
        );
    }

    #[test]
    fn mcp_server_crd_is_serde_round_trippable() {
        // Round-trip through serde_yaml as `kubectl apply` would. A
        // malformed CRD (e.g. `enum` set on a non-string node) would
        // fail to serialize. ValidationRule is part of the public
        // schema and must survive.
        let crd = mcp_server_crd();
        let y = serde_yaml::to_string(&crd).expect("serializes");
        assert!(y.contains("x-kubernetes-validations"));
        assert!(y.contains("productionMode"));
    }

    #[test]
    fn tool_policy_crd_is_serde_round_trippable() {
        let crd = tool_policy_crd();
        let y = serde_yaml::to_string(&crd).expect("serializes");
        assert!(y.contains("x-kubernetes-validations"));
        assert!(y.contains("sandboxMatchLabels"));
    }

    #[test]
    fn a2a_agent_validations_are_non_empty() {
        assert!(!a2a_agent_validations().is_empty());
    }

    #[test]
    fn every_a2a_agent_rule_has_message_and_rule() {
        for rule in a2a_agent_validations() {
            assert!(!rule.rule.is_empty(), "rule body must not be empty");
            let msg = rule.message.as_deref().unwrap_or("");
            assert!(!msg.is_empty(), "rule '{}' missing message", rule.rule);
        }
    }

    #[test]
    fn a2a_agent_crd_has_spec_validations_after_injection() {
        let crd = a2a_agent_crd();
        let v = spec_validations(&crd);
        assert_eq!(v.len(), a2a_agent_validations().len());
    }

    #[test]
    fn a2a_agent_rules_mention_signing_keys_and_eddsa_invariants() {
        let rules: Vec<String> = a2a_agent_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules.iter().any(|r| r.contains("signingKeys")),
            "must enforce signingKeys non-empty; got rules: {rules:?}"
        );
        assert!(
            rules.iter().any(|r| r.contains("EdDSA")),
            "must enforce EdDSA-only signing alg; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("productionMode") && r.contains("https://")),
            "must enforce productionMode -> https endpoint; got rules: {rules:?}"
        );
    }

    #[test]
    fn a2a_agent_crd_is_serde_round_trippable() {
        let crd = a2a_agent_crd();
        let y = serde_yaml::to_string(&crd).expect("serializes");
        assert!(y.contains("x-kubernetes-validations"));
        assert!(y.contains("signingKeys"));
    }

    #[test]
    fn inference_policy_validations_are_non_empty() {
        assert!(!inference_policy_validations().is_empty());
    }

    #[test]
    fn every_inference_policy_rule_has_message_and_rule() {
        for rule in inference_policy_validations() {
            assert!(!rule.rule.is_empty(), "rule body must not be empty");
            let msg = rule.message.as_deref().unwrap_or("");
            assert!(!msg.is_empty(), "rule '{}' missing message", rule.rule);
        }
    }

    #[test]
    fn inference_policy_crd_has_spec_validations_after_injection() {
        let crd = inference_policy_crd();
        let v = spec_validations(&crd);
        assert_eq!(v.len(), inference_policy_validations().len());
    }

    #[test]
    fn inference_policy_rules_mention_token_budget_and_severity_invariants() {
        let rules: Vec<String> = inference_policy_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules
                .iter()
                .any(|r| r.contains("monthlyTokens") && r.contains("dailyTokens")),
            "must enforce monthlyTokens >= dailyTokens; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("Safe") && r.contains("High")),
            "must enforce content-safety severity closed set; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("appliesTo.action") && r.contains("chat")),
            "must enforce appliesTo.action closed set; got rules: {rules:?}"
        );
    }

    #[test]
    fn inference_policy_crd_is_serde_round_trippable() {
        let crd = inference_policy_crd();
        let y = serde_yaml::to_string(&crd).expect("serializes");
        assert!(y.contains("x-kubernetes-validations"));
        assert!(y.contains("monthlyTokens"));
    }

    // ---- ClawMemory (S5) ------------------------------------------------

    #[test]
    fn claw_memory_validations_are_non_empty() {
        assert!(!claw_memory_validations().is_empty());
    }

    #[test]
    fn every_claw_memory_rule_has_message_and_rule() {
        for rule in claw_memory_validations() {
            assert!(!rule.rule.is_empty(), "rule body must not be empty");
            let msg = rule.message.as_deref().unwrap_or("");
            assert!(!msg.is_empty(), "rule '{}' missing message", rule.rule);
        }
    }

    #[test]
    fn claw_memory_crd_has_spec_validations_after_injection() {
        let crd = claw_memory_crd();
        let v = spec_validations(&crd);
        assert_eq!(v.len(), claw_memory_validations().len());
    }

    #[test]
    fn claw_memory_rules_mention_store_name_scope_and_retention_invariants() {
        let rules: Vec<String> = claw_memory_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules
                .iter()
                .any(|r| r.contains("storeName") && r.contains("matches")),
            "must enforce storeName DNS-label shape; got rules: {rules:?}"
        );
        assert!(
            rules.iter().any(|r| r.contains("sandboxRef.name")),
            "must validate sandboxRef.name; got rules: {rules:?}"
        );
        assert!(
            rules.iter().any(|r| r.contains("scope")),
            "must validate scope; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("retentionDays") && r.contains("> 0")),
            "must enforce retentionDays > 0; got rules: {rules:?}"
        );
    }

    #[test]
    fn claw_memory_crd_is_serde_round_trippable() {
        let crd = claw_memory_crd();
        let y = serde_yaml::to_string(&crd).expect("serializes");
        assert!(y.contains("x-kubernetes-validations"));
        assert!(y.contains("storeName"));
        assert!(y.contains("scope"));
    }

    // ---- ClawEval (S6) --------------------------------------------------

    #[test]
    fn claw_eval_validations_are_non_empty() {
        assert!(!claw_eval_validations().is_empty());
    }

    #[test]
    fn every_claw_eval_rule_has_message_and_rule() {
        for rule in claw_eval_validations() {
            assert!(!rule.rule.is_empty(), "rule body must not be empty");
            let msg = rule.message.as_deref().unwrap_or("");
            assert!(!msg.is_empty(), "rule '{}' missing message", rule.rule);
        }
    }

    #[test]
    fn claw_eval_crd_has_spec_validations_after_injection() {
        let crd = claw_eval_crd();
        let v = spec_validations(&crd);
        assert_eq!(v.len(), claw_eval_validations().len());
    }

    #[test]
    fn claw_eval_rules_cover_core_invariants() {
        let rules: Vec<String> = claw_eval_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules.iter().any(|r| r.contains("sandboxRef.name")),
            "must validate sandboxRef.name; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("foundry-evals") && r.contains("evaluators")),
            "must require evaluators for foundry-evals suite; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("schedule") && r.contains("split")),
            "must validate schedule cron shape; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("threshold.score") && r.contains("1.0")),
            "must bound threshold.score to [0,1]; got rules: {rules:?}"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("configMapRef") && r.contains("inline")),
            "must enforce dataset configMapRef/inline mutual exclusion; got rules: {rules:?}"
        );
    }

    #[test]
    fn claw_eval_crd_is_serde_round_trippable() {
        let crd = claw_eval_crd();
        let y = serde_yaml::to_string(&crd).expect("serializes");
        assert!(y.contains("x-kubernetes-validations"));
        assert!(y.contains("sandboxRef"));
        assert!(y.contains("evaluators"));
        assert!(y.contains("threshold"));
    }

    #[test]
    fn injection_returns_none_when_spec_property_missing() {
        // Build a CRD with an empty schema tree to confirm the helper
        // gracefully returns None instead of panicking. We construct a
        // minimal CustomResourceDefinition by hand.
        use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
            CustomResourceDefinitionNames, CustomResourceDefinitionSpec,
            CustomResourceDefinitionVersion,
        };
        let crd = CustomResourceDefinition {
            metadata: Default::default(),
            spec: CustomResourceDefinitionSpec {
                group: "x.example".into(),
                names: CustomResourceDefinitionNames {
                    kind: "X".into(),
                    plural: "xs".into(),
                    ..Default::default()
                },
                scope: "Namespaced".into(),
                versions: vec![CustomResourceDefinitionVersion {
                    name: "v1".into(),
                    served: true,
                    storage: true,
                    schema: None, // <- intentionally absent
                    ..Default::default()
                }],
                ..Default::default()
            },
            status: None,
        };
        assert!(
            inject_spec_validations(crd, vec![]).is_none(),
            "missing schema must yield None, never panic"
        );
    }
}
