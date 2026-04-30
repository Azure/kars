// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! In-tree `PolicyDecisionProvider` implementation on `crate::governance::Governance`.
//!
//! The four-seam trait lives in `providers/policy.rs`; the in-tree
//! implementation is here. Putting the `impl` in the `providers/` module
//! (where every other provider lives) keeps `governance.rs` focused on
//! `Governance` itself — PolicyEngine / TrustManager / RateLimiter /
//! BehaviorMonitor wiring plus the legacy synchronous `evaluate` entry
//! point — and keeps the "what is a provider?" answer in one directory.
//!
//! No wrapper type: the trait is implemented directly on `Governance`.
//! `Arc<Governance>` coerces to `Arc<dyn PolicyDecisionProvider>` with
//! no extra allocation, lock, or duplicated state. The AGT-SDK-native
//! provider lands as a distinct concrete type in a sibling module because
//! it carries its own state (SDK client, tenant config).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::governance::Governance;
use crate::providers::policy::{PolicyDecisionProvider, PolicyError, PolicyRequest, PolicyVerdict};

// (no wrapper struct, no `providers/vendored/` directory) and preserves the
// word "vendored" for `/vendor/` — the patched upstream forks — which is
// where every other use of the term in this repo points.
//
// The AGT-SDK-native provider (`AgtPolicyDecisionProvider`) keeps its own
// concrete type in a sibling module because it carries its own state (SDK
// client handle, tenant config) — i.e. it has real behaviour distinct from
// `Governance`, not just translation.

/// Map a [`PolicyRequest`] onto the `(agent_id, action, extra)` triple
/// consumed by [`Governance::evaluate`]. Free function so both the trait
/// impl and tests can reach it.
fn policy_request_to_legacy_args(request: &PolicyRequest) -> (String, String, Option<Value>) {
    let extra = if request.context.is_empty() && request.payload_digest_hex.is_empty() {
        None
    } else {
        let mut obj = serde_json::Map::with_capacity(request.context.len() + 1);
        for (k, v) in &request.context {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
        if !request.payload_digest_hex.is_empty() {
            obj.insert(
                "payload_digest".to_string(),
                Value::String(request.payload_digest_hex.clone()),
            );
        }
        Some(Value::Object(obj))
    };
    (request.principal.clone(), request.tool.clone(), extra)
}

/// Translate the legacy JSON verdict emitted by [`Governance::evaluate`]
/// into the canonical [`PolicyVerdict`].
fn legacy_verdict_to_policy_verdict(value: Value) -> Result<PolicyVerdict, PolicyError> {
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| PolicyError::Internal("governance verdict missing `action` field".into()))?;
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let matched_rule = value
        .get("matched_rule")
        .and_then(Value::as_str)
        .map(str::to_string);

    match action {
        "allow" => {
            if let Some(rule) = matched_rule {
                Ok(PolicyVerdict::AllowWithLabels(vec![(
                    "matched_rule".to_string(),
                    rule,
                )]))
            } else {
                Ok(PolicyVerdict::Allow)
            }
        }
        "deny" => Ok(PolicyVerdict::Deny {
            reason: if reason.is_empty() {
                "denied by in-tree policy".into()
            } else {
                reason
            },
        }),
        "requires_approval" => Ok(PolicyVerdict::NeedsApproval {
            approver: "human".into(),
            ttl: Duration::from_secs(300),
        }),
        other => Err(PolicyError::Internal(format!(
            "governance verdict emitted unknown action `{other}`"
        ))),
    }
}

/// Inverse of [`legacy_verdict_to_policy_verdict`]. Used by the call sites
/// that still return the `{allowed, reason, rate_limited}` JSON shape to
/// clients (e.g. admin routes) during the Phase 1 policy-trait migration.
pub fn verdict_to_legacy_json(verdict: &PolicyVerdict) -> Value {
    match verdict {
        PolicyVerdict::Allow => json!({
            "allowed": true,
            "action": "allow",
            "decision": "allow",
            "matched_rule": Value::Null,
            "reason": Value::Null,
            "rate_limited": false,
        }),
        PolicyVerdict::AllowWithLabels(labels) => {
            let rule = labels
                .iter()
                .find_map(|(k, v)| (k == "matched_rule").then(|| v.clone()));
            json!({
                "allowed": true,
                "action": "allow",
                "decision": "allow",
                "matched_rule": rule,
                "reason": Value::Null,
                "rate_limited": false,
            })
        }
        PolicyVerdict::Deny { reason } => json!({
            "allowed": false,
            "action": "deny",
            "decision": "deny",
            "matched_rule": Value::Null,
            "reason": reason,
            "rate_limited": false,
        }),
        PolicyVerdict::NeedsApproval { .. } => json!({
            "allowed": false,
            "action": "requires_approval",
            "decision": "requires_approval",
            "matched_rule": Value::Null,
            "reason": "requires approval",
            "rate_limited": false,
        }),
    }
}

#[async_trait]
impl PolicyDecisionProvider for Governance {
    /// `decide` is the four-seam entry point. It is a pure translation of
    /// [`PolicyRequest`] → legacy `(agent_id, action, extra)` → calls
    /// [`Governance::evaluate`] (which does PolicyEngine + TrustManager +
    /// RateLimiter + BehaviorMonitor + audit append) → translates the
    /// legacy JSON verdict back into [`PolicyVerdict`].
    ///
    /// `Governance::evaluate` is synchronous and CPU-bound. Each call takes
    /// microseconds (hash-set / vec scans + one YAML context build + a
    /// `PolicyEngine::evaluate`); we intentionally run it inline in the
    /// async context rather than bouncing through `spawn_blocking`, which
    /// would add a task-schedule round-trip per decision and isn't worth
    /// it at these latencies. If policies grow heavy enough to stall the
    /// runtime we revisit with `tokio::task::block_in_place` at that time.
    async fn decide(&self, request: PolicyRequest) -> Result<PolicyVerdict, PolicyError> {
        let (agent_id, action, extra) = policy_request_to_legacy_args(&request);
        let verdict_json = self.evaluate(&agent_id, &action, extra.as_ref());
        legacy_verdict_to_policy_verdict(verdict_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── PolicyDecisionProvider trait-impl tests ────────────────────────

    #[test]
    fn policy_request_to_legacy_args_passthrough() {
        let req = PolicyRequest {
            principal: "agent://tenant/foo".into(),
            tool: "foundry.chat".into(),
            payload_digest_hex: "abc123".into(),
            context: vec![("tier".into(), "1".into())],
        };
        let (aid, action, extra) = policy_request_to_legacy_args(&req);
        assert_eq!(aid, "agent://tenant/foo");
        assert_eq!(action, "foundry.chat");
        let extra = extra.expect("extra populated");
        assert_eq!(extra.get("tier").and_then(Value::as_str), Some("1"));
        assert_eq!(
            extra.get("payload_digest").and_then(Value::as_str),
            Some("abc123"),
        );
    }

    #[test]
    fn policy_request_to_legacy_args_empty_context_none() {
        let req = PolicyRequest {
            principal: "p".into(),
            tool: "t".into(),
            payload_digest_hex: String::new(),
            context: vec![],
        };
        let (_, _, extra) = policy_request_to_legacy_args(&req);
        assert!(extra.is_none());
    }

    #[test]
    fn legacy_verdict_allow() {
        let v = json!({ "action": "allow" });
        assert_eq!(
            legacy_verdict_to_policy_verdict(v).unwrap(),
            PolicyVerdict::Allow
        );
    }

    #[test]
    fn legacy_verdict_allow_with_rule_becomes_label() {
        let v = json!({ "action": "allow", "matched_rule": "rule-42" });
        match legacy_verdict_to_policy_verdict(v).unwrap() {
            PolicyVerdict::AllowWithLabels(labels) => {
                assert!(labels.contains(&("matched_rule".into(), "rule-42".into())));
            }
            other => panic!("expected AllowWithLabels, got {other:?}"),
        }
    }

    #[test]
    fn legacy_verdict_deny_with_reason() {
        let v = json!({ "action": "deny", "reason": "rate limited" });
        match legacy_verdict_to_policy_verdict(v).unwrap() {
            PolicyVerdict::Deny { reason } => assert!(reason.contains("rate limited")),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn legacy_verdict_deny_without_reason_has_default() {
        let v = json!({ "action": "deny" });
        match legacy_verdict_to_policy_verdict(v).unwrap() {
            PolicyVerdict::Deny { reason } => assert!(!reason.is_empty()),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn legacy_verdict_requires_approval() {
        let v = json!({ "action": "requires_approval" });
        match legacy_verdict_to_policy_verdict(v).unwrap() {
            PolicyVerdict::NeedsApproval { approver, ttl } => {
                assert_eq!(approver, "human");
                assert!(ttl.as_secs() >= 60);
            }
            other => panic!("expected NeedsApproval, got {other:?}"),
        }
    }

    #[test]
    fn legacy_verdict_unknown_action_is_internal_error() {
        assert!(matches!(
            legacy_verdict_to_policy_verdict(json!({ "action": "teleport" })),
            Err(PolicyError::Internal(_))
        ));
    }

    #[test]
    fn legacy_verdict_missing_action_is_internal_error() {
        assert!(matches!(
            legacy_verdict_to_policy_verdict(json!({ "allowed": false })),
            Err(PolicyError::Internal(_))
        ));
    }

    #[test]
    fn verdict_to_legacy_json_allow_roundtrips() {
        let back = legacy_verdict_to_policy_verdict(verdict_to_legacy_json(&PolicyVerdict::Allow))
            .unwrap();
        assert_eq!(back, PolicyVerdict::Allow);
    }

    #[test]
    fn verdict_to_legacy_json_allow_with_labels_preserves_rule() {
        let labels = vec![("matched_rule".to_string(), "r-7".to_string())];
        let back = legacy_verdict_to_policy_verdict(verdict_to_legacy_json(
            &PolicyVerdict::AllowWithLabels(labels.clone()),
        ))
        .unwrap();
        assert_eq!(back, PolicyVerdict::AllowWithLabels(labels));
    }

    #[test]
    fn verdict_to_legacy_json_deny_preserves_reason() {
        let back = legacy_verdict_to_policy_verdict(verdict_to_legacy_json(&PolicyVerdict::Deny {
            reason: "malformed tool arg".into(),
        }))
        .unwrap();
        assert_eq!(
            back,
            PolicyVerdict::Deny {
                reason: "malformed tool arg".into()
            }
        );
    }

    #[test]
    fn verdict_to_legacy_json_needs_approval_roundtrips() {
        let back = legacy_verdict_to_policy_verdict(verdict_to_legacy_json(
            &PolicyVerdict::NeedsApproval {
                approver: "human".into(),
                ttl: Duration::from_secs(300),
            },
        ))
        .unwrap();
        assert!(matches!(back, PolicyVerdict::NeedsApproval { .. }));
    }

    #[tokio::test]
    async fn decide_through_trait_matches_evaluate() {
        let gov = Governance::new("test-sandbox");
        let legacy = gov.evaluate("agent-1", "shell:ls", None);
        let expected = legacy_verdict_to_policy_verdict(legacy).unwrap();

        let req = PolicyRequest {
            principal: "agent-1".into(),
            tool: "shell:ls".into(),
            payload_digest_hex: String::new(),
            context: vec![],
        };
        let actual = gov.decide(req).await.unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn decide_via_arc_dyn_trait_coercion() {
        let gov: Arc<dyn PolicyDecisionProvider> =
            Arc::new(Governance::new("test-sandbox")) as Arc<dyn PolicyDecisionProvider>;
        let req = PolicyRequest {
            principal: "agent-1".into(),
            tool: "foundry.chat".into(),
            payload_digest_hex: String::new(),
            context: vec![],
        };
        let verdict = gov.decide(req).await.unwrap();
        assert!(matches!(
            verdict,
            PolicyVerdict::Allow
                | PolicyVerdict::AllowWithLabels(_)
                | PolicyVerdict::Deny { .. }
                | PolicyVerdict::NeedsApproval { .. }
        ));
    }
}
