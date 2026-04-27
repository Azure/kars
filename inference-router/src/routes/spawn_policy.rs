//! AGT policy gate for `POST /sandbox/spawn`. First production call-site
//! on the four-seam [`crate::providers::PolicyDecisionProvider`] trait.
//!
//! Extracted from `routes/handoff.rs` so `handoff.rs` stays under its
//! §4.2 LOC budget while the provider-seam migration advances. New
//! per-route policy gates land in sibling modules here rather than
//! inflating `handoff.rs`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::errors;
use crate::providers::{PolicyRequest, PolicyVerdict};
use crate::routes::AppState;

/// Run the policy check for creating a new sub-agent sandbox. Returns
/// `Ok(())` to allow, or `Err(response)` to fail-closed with the
/// appropriate status (`403` deny / approval, `503` provider error).
pub async fn check_sandbox_spawn(
    state: &AppState,
    parent: &str,
    child: &str,
) -> Result<(), Response> {
    let request = PolicyRequest {
        principal: parent.to_string(),
        tool: format!("spawn:create:{child}"),
        payload_digest_hex: String::new(),
        context: vec![("child".into(), child.to_string())],
    };
    match state.policy_provider.decide(request).await {
        Ok(PolicyVerdict::Allow | PolicyVerdict::AllowWithLabels(_)) => Ok(()),
        Ok(PolicyVerdict::Deny { reason }) => {
            tracing::warn!(%parent, %child, %reason, "AGT policy DENIED spawn");
            Err(errors::flat(
                StatusCode::FORBIDDEN,
                format!("Spawn blocked by policy: {reason}"),
            )
            .into_response())
        }
        Ok(PolicyVerdict::NeedsApproval { approver, .. }) => {
            tracing::warn!(%parent, %child, %approver, "AGT policy: spawn needs approval");
            Err(errors::flat(
                StatusCode::FORBIDDEN,
                format!("Spawn requires approval by {approver}"),
            )
            .into_response())
        }
        Err(err) => {
            // Fail-closed under §1.3 `OutageMode::Strict` (default).
            tracing::error!(%parent, %child, %err, "policy provider error — failing closed");
            Err(errors::flat(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Policy provider unavailable: {err}"),
            )
            .into_response())
        }
    }
}
