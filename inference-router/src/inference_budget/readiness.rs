// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{proxy::UpstreamConfig, routes::AppState};

pub async fn ready(state: &AppState, upstream: UpstreamConfig) -> anyhow::Result<()> {
    let client = state
        .inference_budget
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("budget binding absent"))?;
    let policy = crate::inference_policy_loader::current_snapshot(&state.inference_policy).await;
    if !policy.guardrails.is_empty() {
        anyhow::bail!("Mandatory standalone moderation needs a supported bounded contract");
    }
    let target = crate::routes::resolve_primary_upstream(state, &policy, upstream)?;
    client.ready_for(&target).await?;
    crate::proxy::credential_for_upstream(&state.auth, Some(&state.copilot), &target).await?;
    Ok(())
}
