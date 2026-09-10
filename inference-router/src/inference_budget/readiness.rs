// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{proxy::UpstreamConfig, routes::AppState};

fn observe<T, E>(stage: &'static str, result: Result<T, E>) -> Result<T, E> {
    result.inspect_err(|_| {
        tracing::warn!(budget_stage = stage, "Governed inference readiness denied");
    })
}

pub async fn ready(state: &AppState, mut upstream: UpstreamConfig) -> anyhow::Result<()> {
    let client = observe(
        "router-binding",
        state
            .inference_budget
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("budget binding absent")),
    )?;
    let policy = crate::inference_policy_loader::current_snapshot(&state.inference_policy).await;
    if !policy.guardrails.is_empty() {
        tracing::warn!(
            budget_stage = "router-guardrails",
            "Governed inference readiness denied"
        );
        anyhow::bail!("Mandatory standalone moderation needs a supported bounded contract");
    }
    observe(
        "router-provider",
        crate::routes::apply_provider_resolution(state, &mut upstream, &policy),
    )?;
    let candidates = crate::failover::candidates_for_request(&upstream, &policy, b"{}");
    let candidate = observe(
        "router-candidate",
        candidates
            .first()
            .ok_or_else(|| anyhow::anyhow!("model route absent")),
    )?;
    let target = observe(
        "router-target",
        crate::failover::resolve_candidate(&upstream, &state.config, candidate),
    )?;
    observe("router-catalog", client.ready_for(&target).await)?;
    observe(
        "router-credential",
        crate::proxy::credential_for_upstream(&state.auth, Some(&state.copilot), &target).await,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::observe;

    #[test]
    fn diagnostic_observation_preserves_success_and_exact_failure() {
        assert_eq!(observe::<_, &str>("router-target", Ok(7)), Ok(7));
        assert_eq!(
            observe::<(), _>("router-target", Err("private-error")),
            Err("private-error")
        );
    }
}
