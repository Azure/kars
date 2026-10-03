// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "execution_plan_tests.rs"]
pub(crate) mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPlan {
    pub schema: String,
    pub roles: Vec<ExecutionRole>,
    pub max_parallel: i32,
    pub synthesis: ExecutionSynthesis,
    #[serde(default)]
    pub deliverables: Vec<ExecutionDeliverable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionRole {
    pub name: String,
    pub objective: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub phases: Vec<ExecutionPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPhase {
    pub name: String,
    pub objective: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tool_calls: Vec<ExecutionRequiredToolCall>,
    #[serde(default)]
    pub min_tool_calls: i32,
    pub max_tool_calls: i32,
    #[serde(default)]
    pub fresh_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionRequiredToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionSynthesis {
    pub objective: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub max_tool_calls: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionDeliverable {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

const EXECUTION_CAPABILITIES: &[&str] = &[
    "filesystem-read",
    "filesystem-write",
    "shell",
    "network",
    "web-search",
    "mcp",
    "memory",
];

fn valid_plan_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn valid_capabilities(capabilities: &[String]) -> bool {
    let mut seen = std::collections::HashSet::new();
    capabilities.iter().all(|capability| {
        EXECUTION_CAPABILITIES.contains(&capability.as_str()) && seen.insert(capability)
    })
}

fn valid_deliverable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && !matches!(name, "response.md" | "constructor" | "prototype")
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn execution_plan_is_acyclic(plan: &ExecutionPlan) -> bool {
    let dependencies = plan
        .roles
        .iter()
        .map(|role| (role.name.as_str(), role.depends_on.as_slice()))
        .collect::<std::collections::HashMap<_, _>>();
    fn visit<'a>(
        role: &'a str,
        dependencies: &std::collections::HashMap<&'a str, &'a [String]>,
        visiting: &mut std::collections::HashSet<&'a str>,
        visited: &mut std::collections::HashSet<&'a str>,
    ) -> bool {
        if visited.contains(role) {
            return true;
        }
        if !visiting.insert(role) {
            return false;
        }
        for dependency in dependencies.get(role).copied().unwrap_or_default() {
            if !visit(dependency, dependencies, visiting, visited) {
                return false;
            }
        }
        visiting.remove(role);
        visited.insert(role);
        true
    }
    let mut visiting = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    plan.roles
        .iter()
        .all(|role| visit(&role.name, &dependencies, &mut visiting, &mut visited))
}

pub fn validate_activation(
    plan: Option<&ExecutionPlan>,
    decomposition: Option<&str>,
    active: bool,
) -> Result<(), String> {
    if decomposition.is_some_and(|marker| marker != "execution-plan/v1")
        || (decomposition.is_some() && plan.is_none())
    {
        return Err("ReviewedExecutionPlanMissing: the recorded decomposition has no supported retained plan; re-review the package before activation".into());
    }
    if let Some(plan) = plan {
        validate_execution_plan(plan)?;
        if active {
            return Err("TypedPlanExecutionUnavailable: the current executor cannot enforce reviewed roles, phases, budgets and deliverables; keep this package paused".into());
        }
    }
    Ok(())
}

pub fn validate_execution_plan(plan: &ExecutionPlan) -> Result<(), String> {
    if plan.schema != "kars.execution-plan/v1" {
        return Err("execution plan schema must be kars.execution-plan/v1".into());
    }
    if plan.roles.is_empty() || plan.roles.len() > 8 {
        return Err("execution plan requires 1-8 roles".into());
    }
    if plan.max_parallel < 1 || plan.max_parallel > plan.roles.len() as i32 {
        return Err("execution plan max_parallel must be within the role count".into());
    }
    let role_names = plan
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    if role_names.len() != plan.roles.len() || role_names.iter().any(|name| !valid_plan_name(name))
    {
        return Err("execution plan role names must be unique DNS-safe labels".into());
    }
    for role in &plan.roles {
        if !(20..=1200).contains(&role.objective.len()) {
            return Err(format!("role {} has an invalid objective", role.name));
        }
        if role.phases.is_empty() || role.phases.len() > 8 {
            return Err(format!("role {} requires 1-8 phases", role.name));
        }
        if role.budget_tokens.is_some_and(|budget| budget <= 0) {
            return Err(format!("role {} has an invalid budget", role.name));
        }
        let mut phase_names = std::collections::HashSet::new();
        for phase in &role.phases {
            if !valid_plan_name(&phase.name) || !phase_names.insert(phase.name.as_str()) {
                return Err(format!("role {} has invalid phase names", role.name));
            }
            if !(20..=1200).contains(&phase.objective.len())
                || phase.min_tool_calls < 0
                || phase.min_tool_calls > phase.max_tool_calls
                || !(0..=32).contains(&phase.max_tool_calls)
                || !valid_capabilities(&phase.capabilities)
            {
                return Err(format!(
                    "role {} phase {} is invalid",
                    role.name, phase.name
                ));
            }
            if phase.required_tool_calls.len() > 8
                || phase.required_tool_calls.len() as i32 > phase.max_tool_calls
            {
                return Err(format!(
                    "role {} phase {} has invalid required tool calls",
                    role.name, phase.name
                ));
            }
            for call in &phase.required_tool_calls {
                if call.name != "github_actions_job_logs"
                    || call.arguments.len() > 8
                    || call
                        .arguments
                        .iter()
                        .any(|(key, value)| key.is_empty() || key.len() > 128 || value.len() > 4096)
                    || !phase
                        .capabilities
                        .iter()
                        .any(|capability| capability == "mcp")
                    || ["owner", "repo", "job_id"].iter().any(|key| {
                        call.arguments
                            .get(*key)
                            .is_none_or(|value| value.trim().is_empty())
                    })
                    || call.arguments.get("tail_lines").is_some_and(|lines| {
                        lines
                            .parse::<u32>()
                            .ok()
                            .is_none_or(|value| !(1..=2000).contains(&value))
                    })
                {
                    return Err(format!(
                        "role {} phase {} has an unsupported required tool call",
                        role.name, phase.name
                    ));
                }
            }
        }
        let mut dependencies = std::collections::HashSet::new();
        if role.depends_on.iter().any(|dependency| {
            dependency == &role.name
                || !role_names.contains(dependency.as_str())
                || !dependencies.insert(dependency)
        }) {
            return Err(format!("role {} has invalid dependencies", role.name));
        }
    }
    if !(20..=1200).contains(&plan.synthesis.objective.len())
        || !(0..=32).contains(&plan.synthesis.max_tool_calls)
        || !valid_capabilities(&plan.synthesis.capabilities)
    {
        return Err("execution plan synthesis is invalid".into());
    }
    let mut deliverables = std::collections::HashSet::new();
    if plan.deliverables.len() > 16
        || plan.deliverables.iter().any(|deliverable| {
            !valid_deliverable_name(&deliverable.name)
                || deliverable.media_type.as_ref().is_some_and(|media_type| {
                    media_type.trim().is_empty()
                        || media_type.len() > 128
                        || media_type.chars().any(char::is_control)
                })
                || !deliverables.insert(deliverable.name.as_str())
        })
    {
        return Err("execution plan deliverables are invalid".into());
    }
    execution_plan_is_acyclic(plan)
        .then_some(())
        .ok_or_else(|| "execution plan dependencies must be acyclic".into())
}
