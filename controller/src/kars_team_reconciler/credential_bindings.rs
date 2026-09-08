// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use kube::api::{ListParams, Patch, PatchParams};
use serde_json::json;

const PENDING: &str = "kars.azure.com/credential-rebind-pending";

pub(super) async fn reconcile(api: &Api<KarsTask>, team: &KarsTeam) -> Result<(), ReconcileError> {
    let Some(desired) = team
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.credential_bindings.as_ref())
    else {
        return Ok(());
    };
    let desired = serde_json::to_value(desired)
        .map_err(|_| ReconcileError::Invalid("Credential binding serialization failed".into()))?;
    for task in api.list(&ListParams::default()).await? {
        if !tasks::owned(&task.metadata, team)
            || task.metadata.deletion_timestamp.is_some()
            || task.annotations().get(ANNOT_TEAM_ROLE).map(String::as_str) != Some("taskforce")
        {
            continue;
        }
        let pending = task
            .annotations()
            .get(PENDING)
            .is_some_and(|value| value == "true");
        let active = task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch);
        if !active && !pending {
            continue;
        }
        let current = serde_json::to_value(
            task.spec
                .blueprint
                .as_ref()
                .and_then(|blueprint| blueprint.credential_bindings.as_ref()),
        )
        .map_err(|_| ReconcileError::Invalid("Credential binding serialization failed".into()))?;
        if current == desired && !pending {
            continue;
        }
        let uid = task
            .uid()
            .ok_or_else(|| ReconcileError::Invalid("Credential run UID missing".into()))?;
        let version = task.resource_version().ok_or_else(|| {
            ReconcileError::Invalid("Credential run resourceVersion missing".into())
        })?;
        if active {
            api.patch(
                &task.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata":{"uid":uid,"resourceVersion":version,"annotations":{PENDING:"true"}},
                    "spec":{"execution":{"launch":false}}
                })),
            )
            .await?;
            continue;
        }
        if task
            .status
            .as_ref()
            .is_none_or(|status| status.execution_phase.as_deref() != Some("Idle"))
        {
            continue;
        }
        api.patch(&task.name_any(),&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":uid,"resourceVersion":version,"annotations":{PENDING:null}},
            "spec":{"blueprint":{"credentialBindings":desired},"execution":{"launch":!team.spec.paused}}
        }))).await?;
    }
    Ok(())
}
