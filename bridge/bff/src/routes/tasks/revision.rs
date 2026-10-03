// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use kube::ResourceExt;
use std::collections::BTreeMap;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::{cluster::Cluster, task::KarsTask};
use crate::routes::ownership::{require_owned_task, task_is_owned_by};

const REQUESTED: &str = "kars.azure.com/run-requested";
const COMPLETED: &str = "kars.azure.com/run-completed";

/// Fence multi-read responses without rejecting unrelated status/RV updates.
pub(super) struct ReadRevision {
    namespace: String,
    name: String,
    uid: String,
    requested: Option<String>,
    completed: Option<String>,
}

impl ReadRevision {
    pub(super) fn capture(
        task: &KarsTask,
        ns: &str,
        name: &str,
        principal: &Principal,
    ) -> AppResult<Self> {
        if !task_is_owned_by(task, principal)
            || task.metadata.deletion_timestamp.is_some()
            || task.namespace().as_deref() != Some(ns)
            || task.name_any() != name
        {
            return Err(AppError::NotFound);
        }
        Ok(Self {
            namespace: ns.into(),
            name: name.into(),
            uid: task
                .uid()
                .filter(|uid| !uid.is_empty())
                .ok_or(AppError::NotFound)?,
            requested: task.annotations().get(REQUESTED).cloned(),
            completed: task.annotations().get(COMPLETED).cloned(),
        })
    }

    pub(super) fn require_download_nonce(&self, nonce: &str) -> AppResult<()> {
        if nonce.is_empty()
            || self.requested.as_deref() != Some(nonce)
            || self.completed.as_deref() != Some(nonce)
        {
            return Err(AppError::Conflict(
                "mission revision changed or is not complete; refresh the mission".into(),
            ));
        }
        Ok(())
    }

    pub(super) async fn recheck(
        &self,
        cluster: &Cluster,
        principal: &Principal,
    ) -> AppResult<KarsTask> {
        let task = require_owned_task(cluster, &self.namespace, &self.name, principal).await?;
        let current = Self::capture(&task, &self.namespace, &self.name, principal)?;
        if current.uid != self.uid
            || current.requested != self.requested
            || current.completed != self.completed
        {
            return Err(AppError::Conflict(
                "mission revision changed while reading evidence; refresh the mission".into(),
            ));
        }
        Ok(task)
    }
}

/// The current-record store is scoped to kars-system; never join it by name alone.
pub(super) fn output_matches_task(task: &KarsTask, output: &BTreeMap<String, String>) -> bool {
    task.namespace().as_deref() == Some("kars-system")
        && task.metadata.deletion_timestamp.is_none()
        && task
            .uid()
            .is_some_and(|uid| !uid.is_empty() && output.get("taskUid") == Some(&uid))
        && output.get("taskName") == Some(&task.name_any())
        && task.annotations().get(REQUESTED).is_some_and(|nonce| {
            !nonce.is_empty()
                && task.annotations().get(COMPLETED) == Some(nonce)
                && output.get("assignmentNonce") == Some(nonce)
        })
}
