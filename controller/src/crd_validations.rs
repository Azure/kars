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
/// 1. `commerce.dailyCap <= commerce.monthlyCap` (Phase 1 §7 entry 12
///    explicit example).
/// 2. `commerce.dailyCap >= 0` and `commerce.monthlyCap >= 0` (no
///    negative caps; CEL `>=` would silently accept them otherwise).
/// 3. `appliesTo.matchLabels` non-empty (a policy that matches nothing
///    is almost always an authoring mistake; if truly intended, an
///    explicit `disabled: true` field is the right escape hatch — not
///    silently empty selectors).
#[must_use]
pub fn tool_policy_validations() -> Vec<ValidationRule> {
    vec![
        ValidationRule {
            rule: "!has(self.commerce) || self.commerce.dailyCap <= self.commerce.monthlyCap"
                .into(),
            message: Some(
                "spec.commerce.dailyCap must be <= spec.commerce.monthlyCap".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "!has(self.commerce) || (self.commerce.dailyCap >= 0 && self.commerce.monthlyCap >= 0)"
                .into(),
            message: Some(
                "spec.commerce.{dailyCap,monthlyCap} must be non-negative".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
        ValidationRule {
            rule: "has(self.appliesTo.matchLabels) && size(self.appliesTo.matchLabels) > 0".into(),
            message: Some(
                "spec.appliesTo.matchLabels must contain at least one label".into(),
            ),
            reason: Some("FieldValueInvalid".into()),
            ..ValidationRule::default()
        },
    ]
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
    fn tool_policy_rules_mention_daily_le_monthly_cap() {
        let rules: Vec<String> = tool_policy_validations()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert!(
            rules
                .iter()
                .any(|r| r.contains("dailyCap") && r.contains("monthlyCap")),
            "must enforce dailyCap <= monthlyCap; got rules: {rules:?}"
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
        assert!(y.contains("dailyCap"));
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
