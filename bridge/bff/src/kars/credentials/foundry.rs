// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

const PROJECT: &str = "kars.azure.com/foundry-project";
const PROVIDERS: &str = "kars-inference-providers";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct FoundryService {
    pub name: String,
    pub category: Option<String>,
}

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct FoundryProject {
    pub project_endpoint: String,
    pub inference_endpoint: String,
    pub api_key_auth: bool,
    #[serde(default)]
    pub services: Option<Vec<FoundryService>>,
    #[serde(default)]
    pub services_discovered_at: Option<String>,
}

pub(crate) fn saved_foundry_project(
    secret: &Secret,
) -> Result<Option<Option<FoundryProject>>, kube::Error> {
    secret
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(PROJECT))
        .map(|raw| {
            serde_json::from_str(raw).map_err(|_| failure("Saved Foundry project is malformed"))
        })
        .transpose()
}

type CatalogPatch = (
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    json_patch::Patch,
);

fn catalog_patch(
    current: &Secret,
    project: Option<&FoundryProject>,
    models: &[String],
    api_key: Option<&str>,
) -> Result<CatalogPatch, kube::Error> {
    if let Some(project) = project
        && (project.api_key_auth != api_key.is_some()
            || api_key.is_some_and(|key| key.trim().is_empty()))
    {
        return Err(failure("Foundry authentication and credential must agree"));
    }
    let uid = current
        .uid()
        .ok_or_else(|| failure("Integration UID missing"))?;
    let version = current
        .resource_version()
        .ok_or_else(|| failure("Integration resourceVersion missing"))?;
    let before = current
        .data
        .as_ref()
        .into_iter()
        .flatten()
        .map(|(key, value)| String::from_utf8(value.0.clone()).map(|value| (key.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(|_| failure("Integration store contains non-UTF8 values"))?;
    let mut after = before.clone();
    for key in [
        "KARS_PROVIDER_FOUNDRY_ENDPOINT",
        "KARS_PROVIDER_FOUNDRY_MODELS",
        "KARS_PROVIDER_FOUNDRY_API_KEY",
        "KARS_PROVIDER_FOUNDRY_TOKEN",
    ] {
        after.remove(key);
    }
    if let Some(project) = project {
        after.insert(
            "KARS_PROVIDER_FOUNDRY_ENDPOINT".into(),
            project.inference_endpoint.clone(),
        );
        after.insert("KARS_PROVIDER_FOUNDRY_MODELS".into(), models.join(","));
        if let Some(key) = api_key {
            after.insert("KARS_PROVIDER_FOUNDRY_API_KEY".into(), key.into());
        }
    }
    let data: BTreeMap<_, _> = after
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                k8s_openapi::ByteString(value.as_bytes().to_vec()),
            )
        })
        .collect();
    let mut annotations = current.metadata.annotations.clone().unwrap_or_default();
    // Explicit null remembers a discovery disconnect without modifying a legacy
    // controller project that may still back existing team memory.
    annotations.insert(
        PROJECT.into(),
        serde_json::to_string(&project).map_err(|_| failure("Foundry project encoding failed"))?,
    );
    let patch = serde_json::from_value(json!([
        {"op":"test","path":"/metadata/uid","value":uid},
        {"op":"test","path":"/metadata/resourceVersion","value":version},
        {"op":"add","path":"/metadata/annotations","value":annotations},
        {"op":"add","path":"/data","value":data}
    ]))
    .map_err(|_| failure("Foundry catalogue patch encoding failed"))?;
    Ok((before, after, patch))
}

impl Cluster {
    pub(crate) async fn write_foundry_catalog(
        &self,
        expected: &Secret,
        project: Option<&FoundryProject>,
        models: &[String],
        api_key: Option<&str>,
    ) -> Result<(), kube::Error> {
        let namespace = self.core_namespace();
        let (_, current) = self.integration_store(&namespace, PROVIDERS).await?;
        if current.uid() != expected.uid()
            || current.resource_version() != expected.resource_version()
        {
            return Err(kube::Error::Api(kube::core::Status {
                status: Some(kube::core::response::StatusSummary::Failure), reason: "Conflict".into(), code: 409,
                message: "Provider configuration changed during Foundry discovery; refresh and try again. Nothing was saved.".into(),
                ..Default::default()
            }.boxed()));
        }
        let (before, after, patch) = catalog_patch(&current, project, models, api_key)?;
        self.validate_provider_update(&current, &before, &after)
            .await?;
        Api::<Secret>::namespaced(self.client.clone(), &namespace)
            .patch(
                PROVIDERS,
                &PatchParams::default(),
                &Patch::Json::<Secret>(patch),
            )
            .await
            .map_err(|e| safe("Save enrolled Foundry catalogue", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Secret {
        serde_json::from_value(json!({
            "apiVersion":"v1", "kind":"Secret",
            "metadata":{"name":PROVIDERS,"uid":"provider-uid","resourceVersion":"7","annotations":{"kars.azure.com/default-model":"unchanged"}},
            "data":{"KARS_PROVIDER_AZURE_ENDPOINT":"aHR0cHM6Ly9henVyZS8=","KARS_PROVIDER_FOUNDRY_API_KEY":"b2xk"}
        })).unwrap()
    }

    #[test]
    fn foundry_catalog_is_atomic_and_keeps_unrelated_configuration() {
        let original = secret();
        let project = FoundryProject {
            project_endpoint: "https://r.services.ai.azure.com/api/projects/p".into(),
            inference_endpoint: "https://r.services.ai.azure.com/".into(),
            api_key_auth: true,
            services: Some(vec![FoundryService {
                name: "search".into(),
                category: Some("CognitiveSearch".into()),
            }]),
            services_discovered_at: Some("2026-11-03T00:00:00Z".into()),
        };
        let (_, after, patch) = catalog_patch(
            &original,
            Some(&project),
            &["model".into()],
            Some("replacement"),
        )
        .unwrap();
        let mut value = serde_json::to_value(&original).unwrap();
        json_patch::patch(&mut value, &patch).unwrap();
        let updated: Secret = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(value["metadata"]["annotations"][MODEL_DEFAULT], "unchanged");
        assert!(
            !value["metadata"]["annotations"]
                .to_string()
                .contains("replacement")
        );
        assert_eq!(
            updated.data.as_ref().unwrap()["KARS_PROVIDER_AZURE_ENDPOINT"],
            original.data.unwrap()["KARS_PROVIDER_AZURE_ENDPOINT"]
        );
        assert_eq!(after["KARS_PROVIDER_FOUNDRY_API_KEY"], "replacement");
        let saved = saved_foundry_project(&updated).unwrap().unwrap().unwrap();
        assert_eq!(
            saved.services.unwrap()[0].category.as_deref(),
            Some("CognitiveSearch")
        );
        assert_eq!(
            saved.services_discovered_at.as_deref(),
            Some("2026-11-03T00:00:00Z")
        );
        assert_eq!(
            saved_foundry_project(&updated)
                .unwrap()
                .unwrap()
                .unwrap()
                .project_endpoint,
            project.project_endpoint
        );
    }

    const MODEL_DEFAULT: &str = "kars.azure.com/default-model";

    #[test]
    fn foundry_catalog_rejects_changed_version_or_replaced_store() {
        let (_, _, patch) = catalog_patch(&secret(), None, &[], None).unwrap();
        for (field, replacement) in [("uid", "new-uid"), ("resourceVersion", "8")] {
            let mut value = serde_json::to_value(secret()).unwrap();
            value["metadata"][field] = json!(replacement);
            assert!(json_patch::patch(&mut value, &patch).is_err());
        }
    }

    #[test]
    fn foundry_disconnect_is_distinct_from_legacy_configuration() {
        let original = secret();
        assert!(saved_foundry_project(&original).unwrap().is_none());
        let (_, after, patch) = catalog_patch(&original, None, &[], None).unwrap();
        assert!(!after.contains_key("KARS_PROVIDER_FOUNDRY_API_KEY"));
        let mut value = serde_json::to_value(original).unwrap();
        json_patch::patch(&mut value, &patch).unwrap();
        assert!(
            saved_foundry_project(&serde_json::from_value(value).unwrap())
                .unwrap()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn foundry_catalog_rejects_inconsistent_authentication() {
        for (api_key_auth, key) in [(true, None), (false, Some("key")), (true, Some(" "))] {
            let project = FoundryProject {
                api_key_auth,
                ..Default::default()
            };
            assert!(catalog_patch(&secret(), Some(&project), &[], key).is_err());
        }
    }

    #[test]
    fn foundry_identity_clears_only_its_own_stale_credential() {
        let project = FoundryProject {
            project_endpoint: "project".into(),
            inference_endpoint: "inference".into(),
            api_key_auth: false,
            ..Default::default()
        };
        let (_, after, _) = catalog_patch(&secret(), Some(&project), &[], None).unwrap();
        assert!(!after.contains_key("KARS_PROVIDER_FOUNDRY_API_KEY"));
        assert!(after.contains_key("KARS_PROVIDER_AZURE_ENDPOINT"));
    }
}
