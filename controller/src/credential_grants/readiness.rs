// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only source-authority gate before publishing ordinary Task readiness.

use crate::{
    kars_task::{KarsTask, KarsTaskStatus},
    status::{conditions, phase::{PHASE_DEGRADED, PHASE_READY}},
};
use kube::{Client, ResourceExt};

#[cfg(test)]
mod tests;

pub(crate) async fn preflight(client: &Client, task: &KarsTask) -> Result<(), String> {
    let Some(blueprint) = task.spec.blueprint.as_ref() else {
        return Ok(());
    };
    if let Some(bindings) = blueprint.credential_bindings.as_ref() {
        super::sources::preflight_task(client, task, bindings).await?;
    }
    if let Some(binding) = blueprint.github_binding.as_ref() {
        let workspace = task.namespace().ok_or("Credential Task workspace missing")?;
        super::github::preflight_binding(client, &workspace, binding).await?;
    }
    Ok(())
}

pub(crate) fn selected(task: &KarsTask) -> bool {
    task.spec.blueprint.as_ref().is_some_and(|blueprint| {
        blueprint.credential_bindings.is_some() || blueprint.github_binding.is_some()
    })
}

pub(crate) async fn enforce(client: &Client, task: &KarsTask, status: &mut KarsTaskStatus) {
    if status.phase.as_deref() != Some(PHASE_READY) {
        return;
    }
    if let Err(error) = preflight(client, task).await {
        status.phase = Some(PHASE_DEGRADED.into());
        status.envelope_digest = None;
        let prior = task.status.as_ref()
            .and_then(|status| status.conditions.as_ref())
            .and_then(|conditions| conditions::find(conditions, conditions::TYPE_READY));
        let condition = conditions::preserve_transition_time(
            prior,
            conditions::TYPE_READY,
            conditions::status::FALSE,
            "CredentialAuthorityUnavailable",
            &error,
            task.metadata.generation,
        );
        conditions::set(status.conditions.get_or_insert_with(Vec::new), condition);
    }
}

pub(crate) async fn pause(client: &Client, task: &KarsTask, status: &mut KarsTaskStatus) {
    status.execution_phase = Some(PHASE_DEGRADED.into());
    match crate::kars_task_execution::pause_credentials(client, task).await {
        Ok(exists) => {
            status.sandbox_ref = exists.then(|| crate::mcp_server::LocalObjectRef { name: task.name_any() });
            status.execution_detail = Some(
                "Governed execution authority unavailable; runtime paused without deleting namespace or state".into(),
            );
        }
        Err(error) => {
            status.sandbox_ref = task.status.as_ref().and_then(|status| status.sandbox_ref.clone());
            status.execution_detail = Some(format!("Credential authority unavailable; owned execution pause failed: {error}"));
        }
    }
}
