// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    cluster::Cluster,
    credential_contract::{CredentialBindings, Identity, Selection, Target},
    credential_transport::{failure, object_api, safe},
    credentials::input_name,
    task::TaskExecution,
};
use kube::{
    ResourceExt,
    api::{DynamicObject, Patch, PatchParams},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn legacy_v1(object: &DynamicObject) -> Result<bool, kube::Error> {
    let reference = &object.data["spec"]["credentialsRef"];
    if reference.is_null() {
        return Ok(false);
    }
    let name = reference["name"]
        .as_str()
        .ok_or_else(|| failure("Credential reference is malformed"))?;
    let uid = reference["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| failure("Credential reference UID is missing"))?;
    if object.data["spec"]["credentialBindings"].is_object() {
        if !name.starts_with("kars-credential-bundle-")
            || object
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/credential-bundle-uid"))
                .map(String::as_str)
                != Some(uid)
        {
            return Err(failure(
                "Mixed or unowned internal credential references require explicit repair",
            ));
        }
        return Ok(false);
    }
    if name
        .strip_prefix("kars-credential-source-")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.as_bytes()[0].is_ascii_alphanumeric()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && name.len() <= 253
    {
        return Ok(true);
    }
    Err(failure(
        "Unbound internal credential references are not a legacy migration approval",
    ))
}

pub(super) fn reviewed_bindings(
    object: &DynamicObject,
    target: &Target,
    grant: &Identity,
) -> Result<CredentialBindings, kube::Error> {
    if object.uid().as_deref() != Some(target.uid.as_str())
        || object.metadata.deletion_timestamp.is_some()
        || object.namespace().as_deref() != Some(target.namespace.as_str())
    {
        return Err(failure("Credential target identity or lifecycle changed"));
    }
    if target.kind == "KarsSandbox" && legacy_v1(object)? {
        return Err(failure(
            "A v1 credential consumer requires explicit validated migration, not workspace rebinding",
        ));
    }
    let existing = if target.kind == "KarsSandbox" {
        &object.data["spec"]["credentialBindings"]
    } else {
        &object.data["spec"]["blueprint"]["credentialBindings"]
    };
    let bindings: CredentialBindings = if existing.is_null() {
        CredentialBindings {
            grant: grant.clone(),
            sources: Vec::new(),
        }
    } else {
        serde_json::from_value(existing.clone())
            .map_err(|_| failure("Existing credential binding is invalid"))?
    };
    if bindings.grant != *grant {
        return Err(failure("Target is bound to a different grant UID"));
    }
    if bindings.sources.len() > 3 {
        return Err(failure("Credential binding has too many source scopes"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for source in &bindings.sources {
        if !["workspace", "team", "target"].contains(&source.scope.as_str())
            || !seen.insert(source.scope.as_str())
            || !source.source.name.starts_with("kars-credential-input-")
            || source.source.uid.is_empty()
            || (source.scope != "workspace"
                && source.owner.as_ref().is_none_or(|owner| {
                    owner.uid.is_empty() || owner.namespace != target.namespace
                }))
        {
            return Err(failure(
                "Existing credential authority is malformed or ambiguous",
            ));
        }
    }
    Ok(bindings)
}

pub(super) fn planned_selection(
    object: &DynamicObject,
    target: &Target,
    grant: &Identity,
    mut selection: Selection,
) -> Result<Option<Value>, kube::Error> {
    let mut bindings = reviewed_bindings(object, target, grant)?;
    let existing = if target.kind == "KarsSandbox" {
        &object.data["spec"]["credentialBindings"]
    } else {
        &object.data["spec"]["blueprint"]["credentialBindings"]
    };
    if let Some(old) = bindings
        .sources
        .iter()
        .find(|old| old.scope == selection.scope)
    {
        if old.source != selection.source || old.owner != selection.owner {
            return Err(failure(
                "Source replacement requires explicit operator review",
            ));
        }
        selection.keys.extend(old.keys.clone());
    }
    selection.keys.sort();
    selection.keys.dedup();
    bindings.sources.retain(|old| old.scope != selection.scope);
    bindings.sources.push(selection);
    bindings
        .sources
        .sort_by_key(|source| match source.scope.as_str() {
            "workspace" => 0,
            "team" => 1,
            _ => 2,
        });
    let value = serde_json::to_value(&bindings)
        .map_err(|_| failure("Credential binding serialization failed"))?;
    if value == *existing {
        return Ok(None);
    }
    if target.kind == "KarsTask" && object.data["spec"]["execution"]["launch"] == true {
        return Err(failure(
            "An active standalone Task requires an explicit governed credential rebind before its key authority changes",
        ));
    }
    Ok(Some(if target.kind == "KarsSandbox" {
        json!({"credentialBindings":bindings})
    } else {
        json!({"blueprint":{"credentialBindings":bindings}})
    }))
}

pub(super) fn activation_unchanged(
    captured: &DynamicObject,
    refreshed: &DynamicObject,
) -> Result<bool, kube::Error> {
    let intent = super::credential_review::intent(captured)?;
    if super::credential_review::intent(refreshed)? == intent {
        return Ok(true);
    }
    let (finalizer, manager) = match captured.types.as_ref().map(|types| types.kind.as_str()) {
        Some("KarsTask") => (
            "kars.azure.com/karstask-cleanup",
            "kars-controller/karstask",
        ),
        Some("KarsTeam") => (
            "kars.azure.com/karsteam-cleanup",
            "kars-controller/karsteam",
        ),
        _ => return Ok(false),
    };
    if captured
        .metadata
        .finalizers
        .as_ref()
        .is_some_and(|values| values.iter().any(|value| value == finalizer))
    {
        return Ok(false);
    }
    let mut normalized = refreshed.clone();
    let Some(finalizers) = normalized.metadata.finalizers.as_mut() else {
        return Ok(false);
    };
    let Some(index) = finalizers.iter().position(|value| value == finalizer) else {
        return Ok(false);
    };
    finalizers.remove(index);
    if finalizers.is_empty() && captured.metadata.finalizers.is_none() {
        normalized.metadata.finalizers = None;
    }
    // The controller's initial SSA cleanup registration changes no authority.
    // Only that exact added finalizer and its field-ownership entry may differ.
    if let Some(fields) = normalized.metadata.managed_fields.as_mut() {
        let registration = json!({
            "apiVersion":"kars.azure.com/v1alpha1", "manager":manager,
            "operation":"Apply", "fieldsType":"FieldsV1",
            "fieldsV1":{"f:metadata":{"f:finalizers":{format!("v:\"{finalizer}\""): {}}}}
        });
        fields.retain(|field| {
            let mut field = field.clone();
            field.time = None;
            serde_json::to_value(field).ok().as_ref() != Some(&registration)
        });
        // Merge patches without a field manager share the creator's "unknown"
        // Update entry. Restore it only when the added finalizer is its sole change.
        for field in fields.iter_mut().filter(|field| {
            field.manager.as_deref() == Some("unknown")
                && field.operation.as_deref() == Some("Update")
                && field.subresource.is_none()
        }) {
            for prior in captured.metadata.managed_fields.iter().flatten() {
                let Ok(mut expected) = serde_json::to_value(prior) else {
                    continue;
                };
                let Ok(mut actual) = serde_json::to_value(&*field) else {
                    continue;
                };
                expected.as_object_mut().unwrap().remove("time");
                actual.as_object_mut().unwrap().remove("time");
                let Some(metadata) = expected
                    .pointer_mut("/fieldsV1/f:metadata")
                    .and_then(Value::as_object_mut)
                else {
                    continue;
                };
                let Some(owners) = metadata
                    .entry("f:finalizers")
                    .or_insert_with(|| json!({".": {}}))
                    .as_object_mut()
                else {
                    continue;
                };
                if owners
                    .insert(format!("v:\"{finalizer}\""), json!({}))
                    .is_some()
                {
                    continue;
                }
                if actual == expected {
                    *field = prior.clone();
                    break;
                }
            }
        }
        if fields.is_empty() && captured.metadata.managed_fields.is_none() {
            normalized.metadata.managed_fields = None;
        }
    }
    Ok(super::credential_review::intent(&normalized)? == intent)
}

impl Cluster {
    pub async fn attach_created_credentials(&self, target: &Target) -> Result<(), kube::Error> {
        let grant = self.credential_grant(&target.namespace).await?;
        let names = [
            ("workspace", input_name("Workspace", "")?),
            (
                if target.kind == "KarsTeam" {
                    "team"
                } else {
                    "target"
                },
                input_name(&target.kind, &target.name)?,
            ),
        ];
        for (scope, name) in names {
            if let Some(source) = grant.sources.iter().find(|source| source.name == name) {
                if source.phase == "Blocked"
                    || source.target.as_ref().is_some_and(|owner| owner != target)
                {
                    return Err(failure(
                        "Staged credential source requires operator review; no foreign target is adopted",
                    ));
                }
                self.bind_credential_selection(
                    target,
                    &grant.identity,
                    Selection {
                        scope: scope.into(),
                        source: Identity {
                            name: source.name.clone(),
                            uid: source.uid.clone(),
                        },
                        keys: source.keys.clone(),
                        owner: if scope == "workspace" {
                            None
                        } else {
                            Some(target.clone())
                        },
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    /// A setup hint, not launch authority: recovery still checks the acknowledged source and fences.
    pub fn keyless_draft_setup_pending(object: &impl serde::Serialize) -> bool {
        let Ok(object) = serde_json::to_value(object) else {
            return false;
        };
        let metadata = &object["metadata"];
        metadata["generation"].as_i64() == Some(1)
            && metadata["deletionTimestamp"].is_null()
            && metadata["annotations"]["kars.azure.com/halted"].is_null()
            && metadata["annotations"]["kars.azure.com/run-requested"].is_null()
            && !object["spec"]["execution"]["launch"]
                .as_bool()
                .unwrap_or(false)
            && matches!(
                object["status"]["executionPhase"].as_str(),
                None | Some("Idle")
            )
            && object["status"]["sandboxRef"].is_null()
            && object["status"]["assignment"].is_null()
            && object["spec"]["blueprint"]["githubBinding"].is_object()
            && object["spec"]["blueprint"]["credentialBindings"].is_null()
    }

    /// Resume only the empty source already acknowledged for an unchanged, never-launched draft.
    pub async fn resume_keyless_draft_credentials(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
        version: &str,
    ) -> Result<DynamicObject, kube::Error> {
        let object = object_api(self, namespace, "KarsTask")
            .get(name)
            .await
            .map_err(|error| safe("Read draft credential setup", error))?;
        if object.uid().as_deref() != Some(uid)
            || object.resource_version().as_deref() != Some(version)
            || !Self::keyless_draft_setup_pending(&object)
        {
            return Err(failure(
                "Credential setup recovery requires the unchanged, never-launched keyless draft",
            ));
        }
        let target = Target {
            kind: "KarsTask".into(),
            namespace: namespace.into(),
            name: name.into(),
            uid: uid.into(),
        };
        let grant = self.credential_grant(namespace).await?;
        let source_name = input_name("KarsTask", name)?;
        let source = grant
            .sources
            .iter()
            .find(|source| {
                source.name == source_name
                    && source.target.as_ref() == Some(&target)
                    && source.keys.is_empty()
                    && source.phase == "Ready"
            })
            .ok_or_else(|| {
                failure(
                    "Draft credential source is not acknowledged; no source was created or changed",
                )
            })?;
        self.bind_credential_selection_reviewed(
            &target,
            &grant.identity,
            Selection {
                scope: "target".into(),
                source: Identity {
                    name: source.name.clone(),
                    uid: source.uid.clone(),
                },
                keys: Vec::new(),
                owner: Some(target.clone()),
            },
            None,
            super::credentials::BindingRetry::Lifecycle(Some(Box::new(object))),
        )
        .await
    }

    pub async fn finish_created_credentials(
        &self,
        target: &Target,
        active: bool,
    ) -> Result<(), kube::Error> {
        let api = object_api(self, &target.namespace, &target.kind);
        if active {
            let captured = api
                .get(&target.name)
                .await
                .map_err(|error| safe("Check reviewed plan before credential attachment", error))?;
            if captured.uid().as_deref() != Some(target.uid.as_str())
                || captured.metadata.deletion_timestamp.is_some()
            {
                return Err(failure(
                    "Created target was replaced or deleted before credential attachment",
                ));
            }
            super::execution_plans::validate(&captured, true).map_err(|error| failure(&error))?;
        }
        self.attach_created_credentials(target).await?;
        let mut object = api
            .get(&target.name)
            .await
            .map_err(|e| safe("Read captured target before activation", e))?;
        if object.uid().as_deref() != Some(target.uid.as_str())
            || object.metadata.deletion_timestamp.is_some()
        {
            return Err(failure(
                "Created target was replaced or deleted before activation",
            ));
        }
        if object.data["spec"]["blueprint"]["githubBinding"].is_object()
            && !object.data["spec"]["blueprint"]["credentialBindings"].is_object()
        {
            self.write_agent_credentials(
                &target.namespace,
                &target.kind,
                &target.name,
                Some(&target.uid),
                BTreeMap::new(),
                Vec::new(),
            )
            .await?;
            object = api
                .get(&target.name)
                .await
                .map_err(|error| safe("Refresh captured keyless consumer", error))?;
            if object.uid().as_deref() != Some(target.uid.as_str())
                || object.metadata.deletion_timestamp.is_some()
            {
                return Err(failure(
                    "Keyless consumer was replaced or deleted before activation",
                ));
            }
        }
        if active {
            super::execution_plans::validate(&object, true).map_err(|error| failure(&error))?;
        }
        // Draft tasks may omit execution entirely; the typed default is inactive.
        // Rewriting an unchanged gate needlessly races controller status updates.
        let spec = match target.kind.as_str() {
            "KarsTask"
                if serde_json::from_value::<Option<TaskExecution>>(
                    object.data["spec"]["execution"].clone(),
                )
                .is_ok_and(|execution| {
                    execution.is_some_and(|execution| execution.launch) == active
                }) =>
            {
                return Ok(());
            }
            "KarsTask" => json!({"execution":{"launch":active}}),
            "KarsTeam" if object.data["spec"]["paused"] == !active => return Ok(()),
            "KarsTeam" => json!({"paused":!active}),
            _ => return Ok(()),
        };
        if object
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        {
            return Err(failure(
                "Created target resourceVersion is missing before activation",
            ));
        }
        let mut conflict_retries = 0;
        loop {
            let metadata = super::execution_plans::activation_fence(&object, active)
                .map_err(|error| failure(&error))?;
            let error = match api
                .patch(
                    &target.name,
                    &PatchParams::default(),
                    &Patch::Merge(json!({
                        "metadata":metadata,"spec":spec,
                    })),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) => error,
            };
            if conflict_retries == 2
                || !matches!(&error, kube::Error::Api(response) if response.code == 409)
            {
                return Err(safe("Activate captured credential target", error));
            }
            let refreshed = api
                .get(&target.name)
                .await
                .map_err(|error| safe("Refresh conflicted credential target", error))?;
            if refreshed.metadata.resource_version == object.metadata.resource_version
                || !activation_unchanged(&object, &refreshed)?
            {
                return Err(safe("Activate captured credential target", error));
            }
            object = refreshed;
            conflict_retries += 1;
            tracing::debug!(
                target = %target.name,
                conflict_retries,
                "Retrying credential activation after a controller lifecycle conflict"
            );
        }
    }
}
