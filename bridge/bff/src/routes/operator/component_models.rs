// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars::task::TaskModel;
use serde_json::json;

pub(crate) const MODEL_SOURCE: &str = "kars.azure.com/model-default-source";

fn policy_name(component: &str) -> AppResult<&'static str> {
    match component {
        "orchestrator" => Ok("bridge-orchestrator-inference"),
        "sre" => Ok("sre-inference"),
        _ => Err(AppError::NotFound),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelDefaultMode {
    Inherit,
    Override,
}

#[derive(serde::Deserialize)]
pub struct SetComponentModelRequest {
    pub mode: ModelDefaultMode,
    pub model: Option<TaskModel>,
    pub uid: String,
    pub resource_version: String,
}

fn same_model(left: &Value, right: &Value) -> bool {
    ["provider", "deployment"].iter().all(|key| {
        left[*key]
            .as_str()
            .is_some_and(|value| !value.is_empty() && right[*key].as_str() == Some(value))
    })
}

fn projection(component: &str, policy: &DynamicObject, cluster_default: &Value) -> Value {
    let inherited = annotation(policy, MODEL_SOURCE).as_deref() == Some("cluster");
    let primary = policy
        .data
        .pointer("/spec/modelPreference/primary")
        .cloned()
        .unwrap_or(Value::Null);
    let desired = if inherited {
        cluster_default.clone()
    } else {
        primary.clone()
    };
    let st = status(policy);
    let ready = policy
        .metadata
        .generation
        .is_some_and(|generation| st["observedGeneration"].as_i64() == Some(generation))
        && st["compiledDigest"].as_str().is_some_and(|digest| {
            !digest.is_empty() && st["loadedDigest"].as_str() == Some(digest)
        })
        && st["conditions"].as_array().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|c| c["type"] == "Ready" && c["status"] == "True")
        })
        && same_model(&primary, &desired);
    let managed = policy
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| !owners.is_empty());
    json!({
        "component":component,"policy":name_of(policy),"mode":if inherited {"inherit"} else {"override"},
        "desired":desired,"primary":primary,"applied":ready,"phase":st["phase"],
        "uid":policy.metadata.uid,"resource_version":policy.metadata.resource_version,
        "editable":!managed && spec(policy).get("bundleRef").is_none_or(Value::is_null),
        "conditions":st["conditions"],
    })
}

pub async fn get_component_models(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let cluster = require_cluster(&state)?;
    let selected = serde_json::to_value(cluster.model_default().await.map_err(upstream)?)
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let mut components = Vec::new();
    for component in ["orchestrator", "sre"] {
        if let Some(policy) = cluster
            .get_kind("kars-system", "InferencePolicy", policy_name(component)?)
            .await
            .map_err(upstream)?
        {
            components.push(projection(component, &policy, &selected));
        }
    }
    Ok(Json(
        json!({"cluster_default":selected,"components":components}),
    ))
}

pub async fn set_component_model(
    State(state): State<AppState>,
    axum::extract::Path(component): axum::extract::Path<String>,
    Json(req): Json<SetComponentModelRequest>,
) -> AppResult<Json<Value>> {
    let cluster = require_cluster(&state)?;
    let name = policy_name(&component)?;
    let current = cluster
        .get_kind("kars-system", "InferencePolicy", name)
        .await
        .map_err(upstream)?
        .ok_or(AppError::NotFound)?;
    if req.uid.is_empty()
        || req.resource_version.is_empty()
        || current.metadata.uid.as_deref() != Some(&req.uid)
        || current.metadata.resource_version.as_deref() != Some(&req.resource_version)
    {
        return Err(AppError::Rejected(
            "This policy changed. Refresh before saving; no newer selection was overwritten."
                .into(),
        ));
    }
    if current
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| !owners.is_empty())
        || spec(&current)
            .get("bundleRef")
            .is_some_and(|v| !v.is_null())
    {
        return Err(AppError::Rejected(
            "Edit this policy's owning resource or signed bundle instead.".into(),
        ));
    }
    let inherited = matches!(req.mode, ModelDefaultMode::Inherit);
    let model = if inherited {
        if req.model.is_some() {
            return Err(AppError::BadRequest(
                "Inherited selection must not include an override".into(),
            ));
        }
        cluster
            .model_default()
            .await
            .map_err(upstream)?
            .ok_or_else(|| {
                AppError::Rejected("Save a cluster default before enabling inheritance.".into())
            })?
    } else {
        req.model
            .ok_or_else(|| AppError::BadRequest("Select a provider and model.".into()))?
    };
    let catalogue = crate::routes::options::build_available_options(cluster).await?;
    if !catalogue
        .models
        .iter()
        .any(|m| m.provider == model.provider && m.deployment == model.deployment)
    {
        return Err(AppError::Rejected(
            "This provider/model is not in the current catalogue. Refresh before selecting it."
                .into(),
        ));
    }
    let patch = json!({"metadata":{"uid":req.uid,"resourceVersion":req.resource_version,
        "annotations":{MODEL_SOURCE:if inherited {Value::String("cluster".into())} else {Value::Null}}},
        "spec":{"modelPreference":{"primary":model}}});
    cluster
        .merge_patch_kind("kars-system", "InferencePolicy", name, patch)
        .await
        .map_err(policies::apply_err)?;
    Ok(Json(
        json!({"saved":true,"note":"Selection saved. Wait for controller compilation and router acknowledgement before it is applied. Existing reviewed tasks keep their model bindings."}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> DynamicObject {
        serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"name":"sre-inference","namespace":"kars-system","uid":"p","resourceVersion":"8","generation":3,"annotations":{MODEL_SOURCE:"cluster"}},"spec":{"modelPreference":{"primary":{"provider":"foundry","deployment":"a"}}},"status":{"observedGeneration":3,"compiledDigest":"sha256:abc","loadedDigest":"sha256:abc","conditions":[{"type":"Ready","status":"True"}]}})).unwrap()
    }
    #[test]
    fn applied_requires_current_desired_route_generation_and_real_echo() {
        let p = policy();
        let model = json!({"provider":"foundry","deployment":"a"});
        assert_eq!(projection("sre", &p, &model)["applied"], true);
        for other in [
            Value::Null,
            json!({"provider":"other","deployment":"a"}),
            json!({"provider":"foundry","deployment":"b"}),
        ] {
            assert_eq!(projection("sre", &p, &other)["applied"], false);
        }
        for (field, value) in [
            ("observedGeneration", json!(2)),
            ("loadedDigest", Value::Null),
            ("compiledDigest", Value::Null),
            ("conditions", json!([])),
        ] {
            let mut p = policy();
            p.data["status"][field] = value;
            assert_eq!(projection("sre", &p, &model)["applied"], false);
        }
    }
    #[test]
    fn explicit_route_does_not_follow_cluster_default() {
        let mut p = policy();
        p.metadata.annotations = None;
        let dto = projection(
            "orchestrator",
            &p,
            &json!({"provider":"other","deployment":"b"}),
        );
        assert_eq!(dto["mode"], "override");
        assert_eq!(dto["desired"]["deployment"], "a");
        assert_eq!(dto["applied"], true);
    }
    #[test]
    fn only_standing_component_names_can_be_selected() {
        assert_eq!(
            policy_name("orchestrator").unwrap(),
            "bridge-orchestrator-inference"
        );
        assert_eq!(policy_name("sre").unwrap(), "sre-inference");
        assert!(policy_name("reviewed-task").is_err());
    }
}
