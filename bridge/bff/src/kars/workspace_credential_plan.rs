use super::{
    cluster::Cluster,
    credential_contract::{Identity, Selection, Target},
    credential_targets::{legacy_v1, planned_selection, reviewed_bindings},
    credential_transport::{failure, object_api, safe},
};
use kube::{
    ResourceExt,
    api::{DynamicObject, ListParams, Patch, PatchParams},
};
use serde_json::json;

pub(super) enum WorkspaceSource {
    Existing(Identity),
    Create { name: String },
}

struct Consumer {
    target: Target,
    version: String,
    snapshot: DynamicObject,
}

pub(super) struct WorkspaceCredentialPlan {
    namespace: String,
    grant: Identity,
    source: WorkspaceSource,
    keys: Vec<String>,
    consumers: Vec<Consumer>,
}

fn selection(source: &Identity, keys: &[String]) -> Selection {
    Selection {
        scope: "workspace".into(),
        source: source.clone(),
        keys: keys.to_vec(),
        owner: None,
    }
}

impl Cluster {
    pub(super) async fn plan_workspace_credentials(
        &self,
        namespace: &str,
        grant: &Identity,
        source: WorkspaceSource,
        keys: Vec<String>,
    ) -> Result<WorkspaceCredentialPlan, kube::Error> {
        let canonical = super::credentials::input_name("Workspace", "")?;
        if match &source {
            WorkspaceSource::Existing(source) => source.name != canonical || source.uid.is_empty(),
            WorkspaceSource::Create { name } => *name != canonical,
        } {
            return Err(failure(
                "Workspace plan requires its canonical source name and any existing exact UID",
            ));
        }
        let mut plan = WorkspaceCredentialPlan {
            namespace: namespace.into(),
            grant: grant.clone(),
            source,
            keys,
            consumers: Vec::new(),
        };
        for kind in ["KarsTeam", "KarsTask", "KarsSandbox"] {
            let objects = object_api(self, namespace, kind)
                .list(&ListParams::default())
                .await
                .map_err(|e| safe("Read workspace credential consumers", e))?;
            for object in objects {
                if object.metadata.deletion_timestamp.is_some() {
                    continue;
                }
                let owned_by = |kind: &str| {
                    object
                        .metadata
                        .owner_references
                        .as_ref()
                        .is_some_and(|owners| {
                            owners
                                .iter()
                                .any(|owner| owner.kind == kind && owner.controller == Some(true))
                        })
                };
                if kind == "KarsSandbox" && owned_by("KarsTask") {
                    continue;
                }
                if kind == "KarsSandbox" && legacy_v1(&object)? {
                    continue;
                }
                if kind == "KarsSandbox"
                    && !object.data["spec"]["credentialBindings"].is_object()
                    && object.data.get("status").is_some_and(|status| {
                        !status.is_null() && status.as_object().is_none_or(|s| !s.is_empty())
                    })
                {
                    continue;
                }
                if kind == "KarsTask"
                    && (owned_by("KarsTeam") || object.data["spec"]["execution"]["launch"] != true)
                {
                    continue;
                }
                let target = Target {
                    kind: kind.into(),
                    namespace: namespace.into(),
                    name: object.name_any(),
                    uid: object
                        .uid()
                        .filter(|uid| !uid.is_empty())
                        .ok_or_else(|| failure("Credential consumer UID missing"))?,
                };
                let version = object
                    .resource_version()
                    .filter(|rv| !rv.is_empty())
                    .ok_or_else(|| failure("Credential consumer resourceVersion missing"))?;
                match &plan.source {
                    WorkspaceSource::Existing(source) => {
                        planned_selection(&object, &target, grant, selection(source, &plan.keys))?;
                    }
                    WorkspaceSource::Create { .. } => {
                        let existing = reviewed_bindings(&object, &target, grant)?;
                        if existing
                            .sources
                            .iter()
                            .any(|source| source.scope == "workspace")
                        {
                            return Err(failure(
                                "A referenced workspace source disappeared or differs; explicit replacement review is required",
                            ));
                        }
                        if kind == "KarsTask" && object.data["spec"]["execution"]["launch"] == true
                        {
                            return Err(failure(
                                "An active standalone Task requires explicit governed rebinding before new source authority",
                            ));
                        }
                    }
                }
                plan.consumers.push(Consumer {
                    target,
                    version,
                    snapshot: object,
                });
            }
        }
        self.recheck_workspace_plan(&plan).await?;
        Ok(plan)
    }

    async fn recheck_workspace_plan(
        &self,
        plan: &WorkspaceCredentialPlan,
    ) -> Result<(), kube::Error> {
        for consumer in &plan.consumers {
            let current = object_api(self, &plan.namespace, &consumer.target.kind)
                .get(&consumer.target.name)
                .await
                .map_err(|e| safe("Recheck complete credential consumer plan", e))?;
            if current.uid().as_deref() != Some(consumer.target.uid.as_str())
                || current.resource_version().as_deref() != Some(consumer.version.as_str())
                || current.metadata.deletion_timestamp.is_some()
            {
                return Err(failure("Credential consumer plan changed before mutation"));
            }
        }
        Ok(())
    }

    pub(super) async fn apply_workspace_plan(
        &self,
        plan: WorkspaceCredentialPlan,
        source: &Identity,
    ) -> Result<(), kube::Error> {
        if source.uid.is_empty()
            || match &plan.source {
                WorkspaceSource::Existing(expected) => source != expected,
                WorkspaceSource::Create { name } => source.name != *name,
            }
        {
            return Err(failure(
                "Created/written source identity differs from the reviewed plan",
            ));
        }
        let mut patches = Vec::new();
        for consumer in &plan.consumers {
            if let Some(spec) = planned_selection(
                &consumer.snapshot,
                &consumer.target,
                &plan.grant,
                selection(source, &plan.keys),
            )? {
                patches.push((consumer, spec));
            }
        }
        // This catches concurrent changes after source creation/write. It is
        // not a multi-resource transaction: a later CAS conflict remains an error.
        self.recheck_workspace_plan(&plan).await?;
        for (consumer, spec) in patches {
            object_api(self,&plan.namespace,&consumer.target.kind).patch(&consumer.target.name,&PatchParams::default(),
                &Patch::Merge(json!({"metadata":{"uid":consumer.target.uid,"resourceVersion":consumer.version},"spec":spec})))
                .await.map_err(|e|safe("Apply reviewed credential consumer plan",e))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn bind_workspace_credentials(
        &self,
        namespace: &str,
        grant: &Identity,
        source: &Identity,
        keys: Vec<String>,
    ) -> Result<(), kube::Error> {
        let plan = self
            .plan_workspace_credentials(
                namespace,
                grant,
                WorkspaceSource::Existing(source.clone()),
                keys,
            )
            .await?;
        self.apply_workspace_plan(plan, source).await
    }
}
