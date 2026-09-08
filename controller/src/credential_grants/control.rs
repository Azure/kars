// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Only core applies constrained provider environment and Bridge rollouts.

use super::*;
use k8s_openapi::api::apps::v1::Deployment;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretKey {
    name: String,
    uid: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvChange {
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    secret: Option<SecretKey>,
    #[serde(default)]
    remove: bool,
}

async fn deployment(
    client: &Client,
    namespace: &str,
    expected: &ObjectIdentity,
) -> Result<Deployment, String> {
    let object = Api::<Deployment>::namespaced(client.clone(), namespace)
        .get(&expected.name)
        .await
        .map_err(|e| api_error("Read enrolled integration Deployment", e))?;
    if identity(&object.metadata)?.0 != expected.uid {
        return Err("Integration Deployment was replaced".into());
    }
    Ok(object)
}

pub(super) async fn reconcile(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<String, String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    let mut revisions = Vec::new();
    for store in &grant.spec.integration_stores {
        if store.purpose == "controller-settings" {
            let reference = grant
                .spec
                .controller
                .as_ref()
                .ok_or("Controller settings require an explicitly enrolled controller UID")?;
            if reference.name != "kars-controller" {
                return Err("Controller settings cannot target another Deployment".into());
            }
            let source = secrets
                .get(&store.secret.name)
                .await
                .map_err(|e| api_error("Read enrolled controller settings", e))?;
            if identity(&source.metadata)?.0 != store.secret.uid {
                return Err("Controller settings store was replaced".into());
            }
            revisions.push(format!(
                "{}:{}:{}",
                store.secret.name,
                store.secret.uid,
                identity(&source.metadata)?.1
            ));
            let Some(raw) = source
                .data
                .as_ref()
                .and_then(|data| data.get("configuration"))
            else {
                continue;
            };
            let changes: Vec<EnvChange> =
                serde_json::from_slice(&raw.0).map_err(|_| "Controller settings are invalid")?;
            let current = deployment(client, &namespace, reference).await?;
            let revision = format!("{}:{}", store.secret.uid, identity(&source.metadata)?.1);
            if current
                .spec
                .as_ref()
                .and_then(|s| s.template.metadata.as_ref())
                .and_then(|m| m.annotations.as_ref())
                .and_then(|a| a.get("kars.azure.com/credential-settings-revision"))
                == Some(&revision)
            {
                continue;
            }
            let mut env = Vec::new();
            let mut unique = std::collections::BTreeSet::new();
            for change in changes {
                if !unique.insert(change.name.clone())
                    || ![
                        "KARS_MODEL_CATALOG",
                        "KARS_TASK_DEFAULT_MODEL",
                        "KARS_TASK_DEFAULT_PROVIDER",
                        "AZURE_OPENAI_DEPLOYMENT",
                        "FOUNDRY_ENDPOINT",
                        "FOUNDRY_PROJECT_ENDPOINT",
                        "FOUNDRY_MEMORY_STORE_ID",
                        "AZURE_OPENAI_API_KEY",
                        "FOUNDRY_API_KEY",
                        "COPILOT_GITHUB_TOKEN",
                        "KARS_INFERENCE_PROVIDER",
                        "KARS_PROVIDER",
                        "AZURE_OPENAI_ENDPOINT",
                    ]
                    .contains(&change.name.as_str())
                    || usize::from(change.remove)
                        + usize::from(change.value.is_some())
                        + usize::from(change.secret.is_some())
                        != 1
                {
                    return Err(
                        "Controller settings contain unsupported or ambiguous environment changes"
                            .into(),
                    );
                }
                if change.remove {
                    env.push(json!({"name":change.name,"$patch":"delete"}));
                } else if let Some(value) = change.value {
                    if value.contains('\0')
                        || [
                            "AZURE_OPENAI_API_KEY",
                            "FOUNDRY_API_KEY",
                            "COPILOT_GITHUB_TOKEN",
                        ]
                        .contains(&change.name.as_str())
                    {
                        return Err("Controller credentials require an enrolled Secret key, never inline values".into());
                    }
                    env.push(json!({"name":change.name,"value":value,"valueFrom":null}));
                } else if let Some(key) = change.secret {
                    let enrolled = grant
                        .spec
                        .integration_stores
                        .iter()
                        .find(|store| store.secret.name == key.name && store.secret.uid == key.uid)
                        .ok_or("Controller credential reference is not enrolled")?;
                    if !integration_keys(&enrolled.purpose, &key.name, &key.key) {
                        return Err(
                            "Controller credential key is outside its enrolled purpose".into()
                        );
                    }
                    env.push(json!({"name":change.name,"value":null,"valueFrom":{"secretKeyRef":{"name":key.name,"key":key.key}}}));
                }
            }
            super::verify(client, grant).await?;
            Api::<Deployment>::namespaced(client.clone(),&namespace).patch(&reference.name,&PatchParams::default(),
                &Patch::Strategic(json!({"metadata":{"uid":reference.uid,"resourceVersion":current.metadata.resource_version},
                    "spec":{"template":{"metadata":{"annotations":{"kars.azure.com/credential-settings-revision":revision}},
                        "spec":{"containers":[{"name":"controller","env":env}]}}}})))
                .await.map_err(|e|api_error("Apply owned controller provider settings",e))?;
        }
        if store.purpose == "teams"
            && let Some(consumers) = &grant.spec.bridge_consumers
        {
            if !(1..=5).contains(&consumers.gateway_replicas) {
                return Err("Teams gateway replica bound is invalid".into());
            }
            let source = secrets
                .get(&store.secret.name)
                .await
                .map_err(|e| api_error("Read enrolled Teams store", e))?;
            if identity(&source.metadata)?.0 != store.secret.uid {
                return Err("Teams store was replaced".into());
            }
            revisions.push(format!(
                "{}:{}:{}",
                store.secret.name,
                store.secret.uid,
                identity(&source.metadata)?.1
            ));
            let enabled = [
                "client-id",
                "tenant-id",
                "client-secret",
                "entra-role-map",
                "bff-internal-secret",
            ]
            .iter()
            .all(|key| {
                source
                    .data
                    .as_ref()
                    .and_then(|d| d.get(*key))
                    .is_some_and(|v| !v.0.is_empty())
            });
            let revision = format!("{}:{}", store.secret.uid, identity(&source.metadata)?.1);
            for (reference, replicas) in [
                (
                    &consumers.gateway,
                    Some(if enabled {
                        consumers.gateway_replicas
                    } else {
                        0
                    }),
                ),
                (&consumers.bff, None),
            ] {
                let current = deployment(client, &namespace, reference).await?;
                if current
                    .spec
                    .as_ref()
                    .and_then(|s| s.template.metadata.as_ref())
                    .and_then(|m| m.annotations.as_ref())
                    .and_then(|a| a.get("kars.azure.com/teams-credential-revision"))
                    == Some(&revision)
                    && replicas
                        .is_none_or(|n| current.spec.as_ref().and_then(|s| s.replicas) == Some(n))
                {
                    continue;
                }
                let mut spec = json!({"template":{"metadata":{"annotations":{"kars.azure.com/teams-credential-revision":revision}}}});
                if let Some(replicas) = replicas {
                    spec["replicas"] = replicas.into();
                }
                Api::<Deployment>::namespaced(client.clone(),&namespace).patch(&reference.name,&PatchParams::default(),
                    &Patch::Merge(json!({"metadata":{"uid":reference.uid,"resourceVersion":current.metadata.resource_version},"spec":spec})))
                    .await.map_err(|e|api_error("Refresh owned Teams consumer",e))?;
            }
        }
    }
    Ok(revisions.join(","))
}
