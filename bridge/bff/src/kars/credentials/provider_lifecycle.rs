// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn in_use(consumer: &str) -> kube::Error {
    kube::Error::Api(kube::core::ErrorResponse {
        status: "Failure".into(),
        reason: "ProviderInUse".into(),
        code: 409,
        message: format!(
            "Provider update would remove a route used by {consumer}. Select a replacement default in Configuration and update the referenced primary/fallback routes in Policies before removing this provider or model. Changing the default does not migrate existing orchestrator, SRE, or mission policies. Nothing was removed."
        ),
    })
}

fn models(keys: &BTreeMap<String, String>, prefix: &str) -> BTreeSet<String> {
    keys.get(&format!("{prefix}_MODELS"))
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

// None means the whole connection is removed; Some retains the remaining models.
fn removed_routes(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> BTreeMap<String, Option<BTreeSet<String>>> {
    let mut prefixes: BTreeSet<String> = before
        .keys()
        .filter_map(|key| {
            key.strip_suffix("_ENDPOINT")
                .filter(|prefix| prefix.starts_with("KARS_PROVIDER_"))
                .map(str::to_string)
        })
        .collect();
    if before.contains_key("COPILOT_GITHUB_TOKEN") {
        prefixes.insert("KARS_PROVIDER_GITHUB_COPILOT".into());
    }
    let mut removed = BTreeMap::new();
    for prefix in prefixes {
        let tag = prefix
            .trim_start_matches("KARS_PROVIDER_")
            .to_ascii_lowercase()
            .replace('_', "-");
        let connected = after
            .get(&format!("{prefix}_ENDPOINT"))
            .is_some_and(|endpoint| !endpoint.trim().is_empty())
            || (tag == "github-copilot" && after.contains_key("COPILOT_GITHUB_TOKEN"));
        let remaining = models(after, &prefix);
        if !connected {
            removed.insert(tag, None);
        } else if !models(before, &prefix).is_subset(&remaining) {
            removed.insert(tag, Some(remaining));
        }
    }
    removed
}

impl Cluster {
    pub(super) async fn validate_provider_update(
        &self,
        secret: &Secret,
        before: &BTreeMap<String, String>,
        after: &BTreeMap<String, String>,
    ) -> Result<(), kube::Error> {
        let removed = removed_routes(before, after);
        let removed_keys: BTreeSet<String> = before
            .keys()
            .filter(|key| !after.contains_key(*key))
            .cloned()
            .collect();
        if !removed_keys.is_empty()
            && let Some((_, settings)) = self
                .optional_integration_store(
                    &self.core_namespace(),
                    "kars-credential-controller-settings",
                )
                .await?
            && let Some(configuration) = settings
                .data
                .as_ref()
                .and_then(|data| data.get("configuration"))
        {
            let entries: Vec<Value> = serde_json::from_slice(&configuration.0).map_err(|_| {
                in_use("malformed controller bootstrap settings; repair them first")
            })?;
            for entry in entries {
                let reference = &entry["secret"];
                if reference["name"] == "kars-inference-providers"
                    && reference["key"]
                        .as_str()
                        .is_some_and(|key| removed_keys.contains(key))
                {
                    return Err(in_use(
                        "controller bootstrap credentials; replace the controller bootstrap binding before removing its key (bootstrap changes are unavailable after private activation)",
                    ));
                }
            }
        }
        if removed.is_empty() {
            return Ok(());
        }
        let affected = |provider: &str, deployment: &str| {
            removed.get(provider).is_some_and(|remaining| {
                remaining
                    .as_ref()
                    .is_none_or(|models| !models.contains(deployment))
            })
        };
        if let Some(raw) = secret
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("kars.azure.com/default-model"))
        {
            let selected: crate::kars::task::TaskModel = serde_json::from_str(raw)
                .map_err(|_| in_use("a malformed saved default; repair it first"))?;
            if affected(&selected.provider, &selected.deployment) {
                return Err(in_use("the saved default"));
            }
        }
        // An unavailable policy API is not evidence that no consumers exist.
        let resource = kube::core::ApiResource::from_gvk(&kube::api::GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "InferencePolicy",
        ));
        let policies: Api<kube::api::DynamicObject> = Api::all_with(self.client.clone(), &resource);
        for policy in policies
            .list(&kube::api::ListParams::default())
            .await?
            .items
        {
            let preference = &policy.data["spec"]["modelPreference"];
            let routes = preference.get("primary").into_iter().chain(
                preference
                    .get("fallback")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            );
            for route in routes {
                if let (Some(provider), Some(deployment)) =
                    (route["provider"].as_str(), route["deployment"].as_str())
                    && affected(provider, deployment)
                {
                    return Err(in_use(&format!(
                        "InferencePolicy {}/{}",
                        policy.namespace().unwrap_or_default(),
                        policy.name_any()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Check before destructive external work (Foundry disconnect / local undeploy).
    /// The store mutation checks again against its own CAS snapshot.
    pub async fn require_provider_removable(&self, tag: &str) -> Result<(), kube::Error> {
        let Some((_, secret)) = self
            .optional_integration_store(&self.core_namespace(), "kars-inference-providers")
            .await?
        else {
            return Ok(());
        };
        let before = secret
            .data
            .as_ref()
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                String::from_utf8(value.0.clone()).map(|value| (key.clone(), value))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|_| failure("Integration store contains non-UTF8 values"))?;
        let mut after = before.clone();
        let prefix = format!(
            "KARS_PROVIDER_{}_",
            tag.to_ascii_uppercase().replace('-', "_")
        );
        after.retain(|key, _| {
            !key.starts_with(&prefix) && !(tag == "github-copilot" && key == "COPILOT_GITHUB_TOKEN")
        });
        self.validate_provider_update(&secret, &before, &after)
            .await
    }
}
