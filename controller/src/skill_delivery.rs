// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::kars_skill::KarsSkill;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SELECTION: &str = "kars.azure.com/skills";
pub const CATALOG_NAME: &str = "kars-approved-skills";
pub const CATALOG_PATH: &str = "/opt/kars-skills";
pub const CATALOG_DIGEST_ENV: &str = "KARS_SKILL_CATALOG_DIGEST";

pub fn validate_selection(names: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !crate::skill_package::valid_name(name) || !seen.insert(name) {
            return Err("Skill selection requires unique DNS labels".into());
        }
    }
    Ok(())
}

/// Resolve each selected package against live, same-namespace review and bytes.
/// Only instructions/assets are added; MCP access still needs an explicit grant.
pub async fn resolve(
    client: &Client,
    namespace: &str,
    names: &[String],
    policy: Option<&str>,
    mcp_servers: &[String],
) -> Result<Vec<Value>, String> {
    validate_selection(names)?;
    let api: Api<KarsSkill> = Api::namespaced(client.clone(), namespace);
    let packages: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let mut result = Vec::new();
    for name in names {
        let skill = api
            .get(name)
            .await
            .map_err(|e| format!("Cannot read selected skill '{name}': {e}"))?;
        let digest = crate::skill_package::approved_digest(&json!(skill))
            .map_err(|e| format!("Skill '{name}': {e}"))?;
        if policy != Some(skill.spec.bounding_policy.as_str())
            || skill
                .spec
                .mcp_servers
                .iter()
                .any(|server| !mcp_servers.contains(server))
        {
            return Err(format!(
                "Skill '{name}' requires its exact bounding policy and explicit MCP server grants"
            ));
        }
        let mut files = BTreeMap::new();
        if skill.spec.package {
            let package = packages
                .get(&format!("karsskill-{name}"))
                .await
                .map_err(|e| format!("Cannot read skill package '{name}': {e}"))?;
            skill.validate_package(&package)?;
            let data = package.data.ok_or("Skill package data disappeared")?;
            for path in &skill.spec.files {
                files.insert(
                    path.clone(),
                    data.get(&path.replace('/', "__"))
                        .ok_or("Skill file disappeared")?
                        .clone(),
                );
            }
        }
        result.push(json!({
            "name":name, "uid":skill.metadata.uid, "generation":skill.metadata.generation,
            "versionDigest":digest, "packageDigest":skill.spec.package_digest,
            "summary":skill.spec.summary, "recipe":skill.spec.recipe, "files":files
        }));
    }
    Ok(result)
}

pub fn catalog(sandbox_uid: &str, skills: Vec<Value>) -> Result<(String, String), String> {
    if sandbox_uid.is_empty() {
        return Err("Skill projection requires a Sandbox UID".into());
    }
    let text = json!({"schema":1,"sandboxUid":sandbox_uid,"skills":skills}).to_string();
    if text.len() + "catalog.json".len() > crate::skill_package::MAX_PACKAGE_BYTES {
        return Err("Combined selected skills exceed the 1 MiB projection bound".into());
    }
    let digest = format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(text.as_bytes())
    );
    Ok((text, digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    #[test]
    fn skill_selection_is_retained_in_effective_authority_and_installed_blueprints() {
        use crate::kars_task::{KarsTaskSpec, TaskBlueprint, TaskModel};
        let mut spec = KarsTaskSpec {
            blueprint: Some(TaskBlueprint {
                skills: vec!["checklist".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let model = TaskModel {
            provider: "azure-foundry".into(),
            deployment: "model".into(),
        };
        assert_eq!(
            spec.authorization_configuration_with_model(&model)["blueprint"]["skills"],
            json!(["checklist"])
        );
        let digest = spec.authorization_digest_with_model(&model);
        spec.blueprint.as_mut().unwrap().skills = vec!["other".into()];
        assert_ne!(digest, spec.authorization_digest_with_model(&model));
        for (generated, installed, paths) in [
            (
                serde_json::to_value(crate::kars_task::KarsTask::crd()).unwrap(),
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../deploy/helm/kars/templates/crd-karstask.yaml"
                ),
                vec!["blueprint/properties/skills"],
            ),
            (
                serde_json::to_value(crate::kars_team::KarsTeam::crd()).unwrap(),
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../deploy/helm/kars/templates/crd-karsteam.yaml"
                ),
                vec![
                    "blueprint/properties/skills",
                    "roster/items/properties/blueprint/properties/skills",
                ],
            ),
        ] {
            let rendered =
                crate::helm_drift::render_helm_template(std::path::Path::new(installed), installed);
            let installed: Value = serde_yaml::from_str(&rendered).unwrap();
            for path in paths {
                let pointer = format!(
                    "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/{path}"
                );
                assert_eq!(
                    generated.pointer(&pointer).unwrap(),
                    installed.pointer(&pointer).unwrap(),
                    "{pointer}"
                );
            }
        }
    }

    #[test]
    fn skill_projection_binds_runtime_identity_and_stays_bounded() {
        assert!(validate_selection(&["checklist".into(), "checklist".into()]).is_err());
        assert!(validate_selection(&["../secret".into()]).is_err());
        assert!(catalog("", vec![]).is_err());
        let (text, digest) = catalog("sandbox-a", vec![json!({"name":"checklist"})]).unwrap();
        assert_eq!(
            digest,
            format!(
                "sha256:{}",
                crate::providers::signing::sha256_hex(text.as_bytes())
            )
        );
        assert_ne!(
            digest,
            catalog("sandbox-b", vec![json!({"name":"checklist"})])
                .unwrap()
                .1
        );
        assert!(
            catalog(
                "sandbox",
                vec![json!("x".repeat(crate::skill_package::MAX_PACKAGE_BYTES))]
            )
            .is_err()
        );
    }
}
