// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Inference-side policy gating via the four-seam
//! [`crate::providers::PolicyDecisionProvider`] trait.
//!
//! Each inference handler (chat completions, responses API, image
//! generation, streaming output check) needs to ask "is this principal
//! allowed to perform `<action>`?" before forwarding to the upstream
//! model. Pre-trait, those sites called `state.governance.evaluate(...)`
//! directly and inspected the legacy `{ allowed, reason, … }` JSON
//! shape. This module routes them through `policy_provider.decide(...)`
//! so any future alternate provider (`AgtPolicyDecisionProvider`,
//! `NullPolicyDecisionProvider`, etc.) applies uniformly.
//!
//! Backend is unchanged for the in-tree path — `impl
//! PolicyDecisionProvider for Governance` ultimately calls
//! `Governance::evaluate` (PolicyEngine + TrustManager + RateLimiter +
//! BehaviorMonitor) just like before.

use crate::providers::policy::PolicyError;
use crate::providers::{PolicyRequest, PolicyVerdict};
use crate::routes::AppState;

/// Outcome of a pre-flight policy check at an inference call site.
///
/// The caller maps these to its own (status, body) shape — image gen,
/// chat completions, responses API, and streaming-output check each
/// use slightly different error envelopes.
pub enum InferenceDecision {
    /// Allowed (with or without informational labels).
    Allow,
    /// Denied with a human-readable reason.
    Deny(String),
}

/// Strict-mode pre-flight: runs `decide()` and returns either
/// [`InferenceDecision::Allow`] or [`InferenceDecision::Deny`].
///
/// Provider errors (`Unreachable` / `Internal`) are translated to
/// `Deny` per the trait contract: under `OutageMode::Strict` the
/// router fails closed on backend outages. The caller does not need
/// to distinguish "policy backend unreachable" from "policy denied"
/// — both are 403s with a clear reason. Provider errors are logged
/// at WARN so operators can spot outage patterns.
///
/// `NeedsApproval` is treated as deny at inference time: out-of-band
/// approval flows for inference are not yet wired (and would require
/// returning a 202 with an approval ticket). That behaviour can be
/// added behind this same helper when it lands.
pub async fn check(state: &AppState, sandbox: &str, action: &str) -> InferenceDecision {
    let request = PolicyRequest {
        principal: sandbox.to_string(),
        tool: action.to_string(),
        payload_digest_hex: String::new(),
        context: Vec::new(),
    };
    match state.policy_provider.decide(request).await {
        Ok(PolicyVerdict::Allow) => InferenceDecision::Allow,
        Ok(PolicyVerdict::AllowWithLabels(_)) => InferenceDecision::Allow,
        Ok(PolicyVerdict::Deny { reason }) => InferenceDecision::Deny(reason),
        Ok(PolicyVerdict::NeedsApproval { .. }) => {
            InferenceDecision::Deny("inference requires out-of-band approval".into())
        }
        Err(e) => {
            tracing::warn!(error = %e, action, sandbox, "policy provider error — failing closed");
            InferenceDecision::Deny(strict_error_reason(&e))
        }
    }
}

/// Map a [`PolicyError`] to a short reason string surfaced to the
/// client. We deliberately do **not** echo the upstream error text —
/// the message goes into a 403 response body and could leak
/// information about the backend (URL, auth posture, etc).
fn strict_error_reason(_e: &PolicyError) -> String {
    "policy backend unavailable".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_error_reason_does_not_echo_inner_message() {
        let r = strict_error_reason(&PolicyError::Unreachable(
            "connect to https://secret-host".into(),
        ));
        assert!(!r.contains("secret-host"));
        assert!(!r.contains("https://"));
        assert_eq!(r, "policy backend unavailable");
    }

    #[test]
    fn strict_error_reason_internal_uniform() {
        let r = strict_error_reason(&PolicyError::Internal("stack trace".into()));
        assert!(!r.contains("stack"));
    }
}
