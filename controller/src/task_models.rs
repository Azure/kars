// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::kars_task::TaskModel;
use serde_json::{Value, json};

/// Bounds apply to the new field only, not existing primary routes or rosters.
pub fn fallback_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "maxItems": 8,
        "items": {
            "type": "object",
            "required": ["deployment", "provider"],
            "properties": {
                "deployment": {"type": "string", "minLength": 1, "maxLength": 253, "pattern": "\\S"},
                "provider": {"type": "string", "minLength": 1, "maxLength": 253, "pattern": "\\S"}
            }
        }
    })
}

pub fn fallback_routes(routes: &[TaskModel], provider: &str, deployment: &str) -> Vec<Value> {
    let mut seen =
        std::collections::HashSet::from([(provider.to_string(), deployment.to_string())]);
    routes
        .iter()
        .filter(|route| !route.provider.trim().is_empty() && !route.deployment.trim().is_empty())
        .filter(|route| seen.insert((route.provider.clone(), route.deployment.clone())))
        .map(|route| json!({"provider": route.provider, "deployment": route.deployment}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_blueprint_preserves_fallbacks_and_authorization_binds_every_route() {
        use crate::kars_task::{
            KarsTaskSpec, TaskBlueprint, blueprint::effective_blueprint_with_model,
        };
        let primary = TaskModel {
            provider: "azure-openai".into(),
            deployment: "primary".into(),
        };
        let spec = KarsTaskSpec {
            objective: "Review".into(),
            blueprint: Some(TaskBlueprint {
                model: Some(primary.clone()),
                model_fallbacks: vec![
                    TaskModel {
                        provider: "account-a".into(),
                        deployment: "model-a".into(),
                    },
                    TaskModel {
                        provider: "account-b".into(),
                        deployment: "model-b".into(),
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let original = serde_json::to_value(&spec).unwrap();
        let effective = effective_blueprint_with_model(&spec, &primary);
        assert_eq!(
            serde_json::to_value(effective).unwrap()["modelFallbacks"],
            original["blueprint"]["modelFallbacks"]
        );
        assert_eq!(
            spec.authorization_configuration_with_model(&primary)["blueprint"]["modelFallbacks"],
            original["blueprint"]["modelFallbacks"]
        );
        let digest = spec.authorization_digest_with_model(&primary);
        for (pointer, value) in [
            (
                "/blueprint/modelFallbacks/0/provider",
                json!("other-account"),
            ),
            (
                "/blueprint/modelFallbacks/0/deployment",
                json!("other-model"),
            ),
            ("/blueprint/modelFallbacks", json!([])),
            (
                "/blueprint/modelFallbacks",
                json!([
                    {"provider":"account-b","deployment":"model-b"},
                    {"provider":"account-a","deployment":"model-a"},
                ]),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            let changed: KarsTaskSpec = serde_json::from_value(changed).unwrap();
            assert_ne!(
                digest,
                changed.authorization_digest_with_model(&primary),
                "{pointer}"
            );
        }
    }

    #[test]
    fn fallback_routes_preserve_order_and_provider_identity() {
        let routes = [
            ("a", "primary"),
            ("b", "same"),
            ("b", "same"),
            ("c", "same"),
            ("", "invalid"),
            ("b", " "),
            ("a", "last"),
        ]
        .map(|(provider, deployment)| TaskModel {
            provider: provider.into(),
            deployment: deployment.into(),
        });
        assert_eq!(
            fallback_routes(&routes, "a", "primary"),
            vec![
                json!({"provider":"b","deployment":"same"}),
                json!({"provider":"c","deployment":"same"}),
                json!({"provider":"a","deployment":"last"}),
            ]
        );
        assert!(fallback_routes(&[], "a", "primary").is_empty());
    }

    #[test]
    fn legacy_blueprint_omits_new_field() {
        let blueprint: crate::kars_task::TaskBlueprint = serde_json::from_value(json!({})).unwrap();
        assert!(blueprint.model_fallbacks.is_empty());
        assert!(
            serde_json::to_value(blueprint)
                .unwrap()
                .get("modelFallbacks")
                .is_none()
        );
    }

    #[test]
    fn fallback_schema_is_bounded_without_changing_primary_or_roster() {
        let schema =
            serde_json::to_value(schemars::schema_for!(crate::kars_task::TaskBlueprint)).unwrap();
        assert_eq!(schema["properties"]["modelFallbacks"]["maxItems"], 8);
        assert_eq!(
            schema["properties"]["modelFallbacks"]["items"]["properties"]["provider"]["maxLength"],
            253
        );
        let team =
            serde_json::to_value(schemars::schema_for!(crate::kars_team::KarsTeamSpec)).unwrap();
        assert!(team["properties"]["roster"].get("maxItems").is_none());
    }
}
