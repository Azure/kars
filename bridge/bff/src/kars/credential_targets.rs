use super::{
    cluster::Cluster,
    credential_contract::{CredentialBindings, Identity, Selection, Target},
    credential_transport::{failure, object_api, safe},
    credentials::input_name,
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

    pub async fn finish_created_credentials(
        &self,
        target: &Target,
        active: bool,
    ) -> Result<(), kube::Error> {
        self.attach_created_credentials(target).await?;
        let api = object_api(self, &target.namespace, &target.kind);
        let mut object = api
            .get(&target.name)
            .await
            .map_err(|e| safe("Read captured target before activation", e))?;
        if object.uid().as_deref() != Some(target.uid.as_str()) {
            return Err(failure("Created target was replaced before activation"));
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
            if object.uid().as_deref() != Some(target.uid.as_str()) {
                return Err(failure("Keyless consumer was replaced before activation"));
            }
        }
        let spec = match target.kind.as_str() {
            "KarsTask" => json!({"execution":{"launch":active}}),
            "KarsTeam" => json!({"paused":!active}),
            _ => return Ok(()),
        };
        api.patch(&target.name, &PatchParams::default(), &Patch::Merge(json!({
            "metadata":{"uid":target.uid,"resourceVersion":object.metadata.resource_version},"spec":spec,
        }))).await.map_err(|e| safe("Activate captured credential target", e))?;
        Ok(())
    }
}
