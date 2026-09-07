// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One effective blueprint for materialization and authorization binding.

use super::{KarsTaskSpec, TaskBlueprint, TaskModel, effective_tool_policy};

pub fn effective_runtime_name(spec: &KarsTaskSpec) -> &str {
    match spec
        .blueprint
        .as_ref()
        .and_then(|b| b.runtime.as_deref())
        .or_else(|| spec.execution.as_ref().and_then(|e| e.runtime.as_deref()))
        .unwrap_or("OpenClaw")
    {
        "MAF" => "MicrosoftAgentFramework",
        runtime => runtime,
    }
}

/// Includes effective instructions, model/provider defaults, every capability
/// reference, and the runtime override precedence used to create the sandbox.
/// Clone the entire blueprint so newly added fields cannot disappear from the
/// authorization digest merely because this normalizer has not changed yet.
pub fn effective_blueprint(spec: &KarsTaskSpec) -> TaskBlueprint {
    effective_blueprint_with_model(spec, &controller_default_model())
}

pub fn effective_blueprint_with_model(
    spec: &KarsTaskSpec,
    default_model: &TaskModel,
) -> TaskBlueprint {
    let mut blueprint = spec.blueprint.clone().unwrap_or_default();
    blueprint.runtime = Some(effective_runtime_name(spec).to_string());
    blueprint.model = Some(match &blueprint.model {
        Some(model) if !model.deployment.trim().is_empty() => TaskModel {
            deployment: model.deployment.clone(),
            provider: if model.provider.trim().is_empty() {
                "azure-openai".into()
            } else {
                model.provider.clone()
            },
        },
        _ => default_model.clone(),
    });
    blueprint.instructions = Some(build_instructions(
        &spec.objective,
        blueprint.instructions.as_deref(),
    ));
    blueprint.tool_policy = effective_tool_policy(spec).map(str::to_string);
    blueprint.isolation = Some(
        blueprint
            .isolation
            .take()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "standard".into()),
    );
    blueprint.memory = blueprint
        .memory
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    blueprint
}

pub fn build_instructions(objective: &str, extra: Option<&str>) -> String {
    let mut instructions = format!("Your objective:\n{}", objective.trim());
    if let Some(extra) = extra.map(str::trim).filter(|s| !s.is_empty()) {
        instructions.push_str("\n\nAdditional instructions:\n");
        instructions.push_str(extra);
    }
    instructions
}

pub fn controller_default_model() -> TaskModel {
    resolve_default_model(
        std::env::var("KARS_TASK_DEFAULT_MODEL").ok().as_deref(),
        std::env::var("AZURE_OPENAI_DEPLOYMENT").ok().as_deref(),
        std::env::var("DEFAULT_MODEL").ok().as_deref(),
        std::env::var("KARS_TASK_DEFAULT_PROVIDER").ok().as_deref(),
    )
}

fn resolve_default_model(
    task: Option<&str>,
    azure: Option<&str>,
    default: Option<&str>,
    provider: Option<&str>,
) -> TaskModel {
    TaskModel {
        deployment: [task, azure, default]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .unwrap_or("gpt-4o-mini")
            .into(),
        provider: provider
            .filter(|s| !s.is_empty())
            .unwrap_or("azure-openai")
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_precedence_is_pure_and_does_not_mutate_process_environment() {
        let builtin = resolve_default_model(None, None, None, None);
        assert_eq!(builtin.deployment, "gpt-4o-mini");
        assert_eq!(builtin.provider, "azure-openai");
        assert_eq!(
            resolve_default_model(None, None, Some("default"), None).deployment,
            "default"
        );
        assert_eq!(
            resolve_default_model(None, Some("azure"), Some("default"), None).deployment,
            "azure"
        );
        let explicit = resolve_default_model(
            Some("task"),
            Some("azure"),
            Some("default"),
            Some("github-models"),
        );
        assert_eq!(explicit.deployment, "task");
        assert_eq!(explicit.provider, "github-models");
        assert_eq!(
            resolve_default_model(Some(""), Some("azure"), None, Some("")).deployment,
            "azure"
        );
    }
}
