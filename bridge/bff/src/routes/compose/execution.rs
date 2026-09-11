use super::client::extract_json;
use super::{ComposeDelegation, ComposeDelegationRole};

fn valid_delegation_role(value: &serde_json::Value) -> Option<ComposeDelegationRole> {
    let name = value.get("name")?.as_str()?.trim().to_ascii_lowercase();
    let objective = value.get("objective")?.as_str()?.trim().to_string();
    if name.is_empty()
        || name.len() > 48
        || objective.len() < 20
        || objective.len() > 600
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return None;
    }
    Some(ComposeDelegationRole { name, objective })
}

pub(super) fn single_agent_delegation() -> ComposeDelegation {
    ComposeDelegation {
        mode: "single-agent".into(),
        roles: Vec::new(),
        max_parallel: 1,
    }
}

pub(super) fn delegation_from_execution_plan(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> ComposeDelegation {
    ComposeDelegation {
        mode: "principal-specialists".into(),
        roles: plan
            .roles
            .iter()
            .map(|role| ComposeDelegationRole {
                name: role.name.clone(),
                objective: role.objective.clone(),
            })
            .collect(),
        max_parallel: plan.max_parallel,
    }
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

const EXECUTION_ROLE_PHASE_BASE_WEIGHT: i64 = 4;

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

fn role_budget_weight(role: &crate::routes::tasks::ExecutionRoleDto) -> i64 {
    role.phases
        .iter()
        .map(|phase| EXECUTION_ROLE_PHASE_BASE_WEIGHT + i64::from(phase.max_tool_calls))
        .sum::<i64>()
        .max(1)
}

pub(super) fn weighted_role_budget_floors(
    total_tokens: i64,
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> Vec<i64> {
    let weights = plan
        .roles
        .iter()
        .map(role_budget_weight)
        .collect::<Vec<_>>();
    let total_weight = weights
        .iter()
        .map(|weight| i128::from(*weight))
        .sum::<i128>();
    let total_tokens_i128 = i128::from(total_tokens);
    let mut floors = weights
        .iter()
        .map(|weight| ((total_tokens_i128 * i128::from(*weight)) / total_weight) as i64)
        .collect::<Vec<_>>();
    let assigned = floors.iter().sum::<i64>();
    let mut remainders = weights
        .iter()
        .enumerate()
        .map(|(index, weight)| {
            (
                (total_tokens_i128 * i128::from(*weight)) % total_weight,
                index,
            )
        })
        .collect::<Vec<_>>();
    remainders.sort_by(
        |(left_remainder, left_index), (right_remainder, right_index)| {
            right_remainder
                .cmp(left_remainder)
                .then(left_index.cmp(right_index))
        },
    );
    for (_, index) in remainders
        .into_iter()
        .take((total_tokens - assigned) as usize)
    {
        floors[index] += 1;
    }
    floors
}

pub(super) fn apply_weighted_role_budget_floors(
    plan: &mut crate::routes::tasks::ExecutionPlanDto,
    total_tokens: i64,
) -> (bool, i64) {
    let mut changed = false;
    let floors = weighted_role_budget_floors(total_tokens, plan);
    for (role, floor) in plan.roles.iter_mut().zip(floors) {
        let floor = floor.max(1);
        if role.budget_tokens.is_none_or(|current| current < floor) {
            role.budget_tokens = Some(floor);
            changed = true;
        }
    }
    let total = plan
        .roles
        .iter()
        .filter_map(|role| role.budget_tokens)
        .sum();
    (changed, total)
}

fn valid_deliverable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn execution_plan_is_acyclic(plan: &crate::routes::tasks::ExecutionPlanDto) -> bool {
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

pub(crate) fn validate_execution_plan(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> Result<(), String> {
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
                || !deliverables.insert(deliverable.name.as_str())
        })
    {
        return Err("execution plan deliverables are invalid".into());
    }
    execution_plan_is_acyclic(plan)
        .then_some(())
        .ok_or_else(|| "execution plan dependencies must be acyclic".into())
}

fn parse_execution_plan_result(
    json: &serde_json::Value,
) -> Result<crate::routes::tasks::ExecutionPlanDto, String> {
    let value = json
        .get("execution_plan")
        .ok_or_else(|| "execution_plan is missing".to_string())?;
    let plan = serde_json::from_value(value.clone())
        .map_err(|error| format!("execution_plan does not match the required schema: {error}"))?;
    validate_execution_plan(&plan)?;
    Ok(plan)
}

pub(super) fn parse_execution_plan(
    json: &serde_json::Value,
) -> Option<crate::routes::tasks::ExecutionPlanDto> {
    parse_execution_plan_result(json).ok()
}

pub(super) fn execution_plan_error_from_raw(raw: &str) -> Option<String> {
    let json =
        extract_json(raw).ok_or_else(|| "response did not contain a JSON object".to_string());
    match json {
        Ok(json) => parse_execution_plan_result(&json).err(),
        Err(error) => Some(error),
    }
}

pub(crate) fn validate_delegation(delegation: &ComposeDelegation) -> Result<(), String> {
    match delegation.mode.as_str() {
        "single-agent" if delegation.roles.is_empty() && delegation.max_parallel == 1 => Ok(()),
        "principal-specialists"
            if (2..=4).contains(&delegation.roles.len())
                && delegation.max_parallel >= 1
                && delegation.max_parallel <= delegation.roles.len() as i32
                && delegation.roles.iter().all(|role| {
                    let value = serde_json::json!({
                        "name": role.name,
                        "objective": role.objective,
                    });
                    valid_delegation_role(&value).is_some()
                }) =>
        {
            let unique = delegation
                .roles
                .iter()
                .map(|role| role.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            (unique.len() == delegation.roles.len())
                .then_some(())
                .ok_or_else(|| "delegation role names must be unique".to_string())
        }
        "single-agent" => {
            Err("single-agent delegation must have no roles and max_parallel=1".into())
        }
        "principal-specialists" => Err(
            "principal-specialists delegation requires 2–4 unique valid leaf roles and a bounded max_parallel"
                .into(),
        ),
        _ => Err("delegation mode must be single-agent or principal-specialists".into()),
    }
}

pub(crate) fn delegation_budget_allocation(
    total_tokens: i64,
    role_count: usize,
) -> Result<(i64, i64), String> {
    if role_count == 0 || total_tokens < role_count as i64 {
        return Err(
            "execution plans require a positive total budget with capacity for every role".into(),
        );
    }
    Ok((total_tokens, total_tokens / role_count as i64))
}
