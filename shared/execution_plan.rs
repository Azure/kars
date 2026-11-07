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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_execution_mode"
    )]
    pub execution_mode: Option<String>,
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_execution_mode"
    )]
    pub execution_mode: Option<String>,
}

pub fn deserialize_execution_mode<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let mode = String::deserialize(deserializer)?;
    if mode != "native" {
        return Err(serde::de::Error::custom(
            "execution mode must be native when present",
        ));
    }
    Ok(Some(mode))
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
    "github-pr",
    "python-execute",
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

pub(crate) fn valid_deliverable_name(name: &str) -> bool {
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

// A reviewed Task delegates to roles, then phases. A standing Team first delegates to a run.
pub const REVIEWED_TASK_DELEGATION_DEPTH: i32 = 2;
pub const REVIEWED_TEAM_DELEGATION_DEPTH: i32 = REVIEWED_TASK_DELEGATION_DEPTH + 1;

pub fn validate_reviewed_delegation_depth(depth: i64, required: i32) -> Result<(), String> {
    if depth < i64::from(required) {
        return Err(format!(
            "ReviewedDelegationDepthInsufficient: this reviewed execution requires delegationDepth >= {required}, but the envelope grants {depth}; re-review the envelope before activation"
        ));
    }
    Ok(())
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
            validate_filesystem_execution(plan)?;
        }
    }
    Ok(())
}

/// Base inputs (revision deliverables and approved Team memory) reach every phase,
/// including independent first phases. Validate again after those inputs are selected,
/// before reserving a run; static dependency validation alone cannot see them.
pub fn validate_retained_input_execution(
    plan: Option<&ExecutionPlan>,
    input_count: usize,
) -> Result<(), String> {
    if input_count == 0 {
        return Ok(());
    }
    let plan = plan.ok_or_else(|| {
        "ReviewedExecutionPlanMissing: retained previous work requires a reviewed execution plan; open Team Edit to review the launch package before running".to_string()
    })?;
    let mut affected = Vec::new();
    let mut check = |label: String, capabilities: &[String], maximum: i32, minimum: usize| {
        if !capabilities
            .iter()
            .any(|capability| capability == "filesystem-read")
            || i64::from(maximum) < minimum as i64
        {
            affected.push(format!(
                "{label} (permission to read previous work and at least {minimum} tool calls required)"
            ));
        }
    };
    for role in &plan.roles {
        for (index, phase) in role.phases.iter().enumerate() {
            let dependencies = if index == 0 { role.depends_on.len() } else { 1 };
            let minimum = if phase.execution_mode.as_deref() == Some("native") {
                1
            } else {
                input_count + dependencies + phase.required_tool_calls.len() + 1
            };
            check(
                format!("role '{}' phase '{}'", role.name, phase.name),
                &phase.capabilities,
                phase.max_tool_calls,
                minimum,
            );
        }
    }
    let minimum = if plan.synthesis.execution_mode.as_deref() == Some("native") {
        1
    } else {
        input_count + plan.roles.len() + plan.deliverables.len()
    };
    check(
        "principal synthesis".into(),
        &plan.synthesis.capabilities,
        plan.synthesis.max_tool_calls,
        minimum,
    );
    if affected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "This run cannot start because some phases cannot read previous work: {}. No permissions were added. Pause the Team, open Edit → Reading previous work, review the read permission and maximum tool calls for the named phases, then Save input permissions and resume. If an administrator's tool policy denies reading, ask the administrator to review that policy; changing these controls cannot override it. Host, network and credential access are unchanged. [ReviewedInputCapabilityRequired]",
            affected.join("; ")
        ))
    }
}

/// Installed executor subset. Each fresh phase exports one immutable handoff; synthesis
/// reads those handoffs and exports the exact reviewed UTF-8 deliverable names.
pub fn validate_filesystem_execution(plan: &ExecutionPlan) -> Result<(), String> {
    validate_execution_plan(plan)?;
    let files = |caps: &[String]| {
        caps.len() == 2
            && caps.iter().any(|c| c == "filesystem-read")
            && caps.iter().any(|c| c == "filesystem-write")
    };
    let objective_valid = |text: &str| {
        text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            .len()
            >= 20
    };
    let mut unsupported = Vec::new();
    for role in &plan.roles {
        if !objective_valid(&role.objective) {
            unsupported.push(format!(
                "role {} objective needs at least 20 trimmed UTF-8 bytes",
                role.name
            ));
        }
        if role
            .budget_tokens
            .is_some_and(|n| n > 9_007_199_254_740_991)
        {
            unsupported.push(format!(
                "role {} budget exceeds the exact wire-integer limit",
                role.name
            ));
        }
        for phase in &role.phases {
            let label = format!("role {} phase {}", role.name, phase.name);
            if !objective_valid(&phase.objective) {
                unsupported.push(format!(
                    "{label} objective needs at least 20 trimmed UTF-8 bytes"
                ));
            }
            let native = phase.execution_mode.as_deref() == Some("native");
            if !phase.fresh_context {
                unsupported.push(format!("{label} requires freshContext=true; cross-phase conversation reuse is not supported"));
            }
            if native {
                if phase.capabilities.iter().any(|c| {
                    !matches!(
                        c.as_str(),
                        "filesystem-read"
                            | "filesystem-write"
                            | "network"
                            | "memory"
                            | "github-pr"
                            | "python-execute"
                    )
                }) {
                    unsupported.push(format!(
                        "{label} requests capabilities without a native Mission adapter"
                    ));
                }
                if phase.capabilities.iter().any(|c| c == "python-execute")
                    && !["filesystem-read", "filesystem-write", "network"]
                        .iter()
                        .all(|required| phase.capabilities.iter().any(|c| c == required))
                {
                    unsupported.push(format!("{label} python-execute requires explicit filesystem-read, filesystem-write and network; code uses the existing agent identity"));
                }
                if !phase.capabilities.iter().any(|c| c == "filesystem-write") {
                    unsupported.push(format!(
                        "{label} needs filesystem-write for its retained handoff"
                    ));
                }
            } else if !files(
                &phase
                    .capabilities
                    .iter()
                    .filter(|c| c.as_str() != "network")
                    .cloned()
                    .collect::<Vec<_>>(),
            ) {
                unsupported.push(format!("{label} requests capabilities {:?}; installed execution requires filesystem-read and filesystem-write with optional exact HTTPS GET network; hosted web-search is outside the finite funding contract", phase.capabilities));
            }
            if let Err(reason) = validate_reviewed_fetch_phase(phase) {
                unsupported.extend(
                    reason
                        .split("; ")
                        .map(|blocker| format!("{label} {blocker}")),
                );
            }
            if phase.max_tool_calls < 1 {
                unsupported.push(format!("{label} needs a positive maxToolCalls"));
            }
        }
    }
    if !objective_valid(&plan.synthesis.objective) {
        unsupported.push("synthesis objective needs at least 20 trimmed UTF-8 bytes".into());
    }
    if plan.roles.is_empty() {
        if plan
            .synthesis
            .capabilities
            .iter()
            .any(|c| c == "python-execute")
            && !["filesystem-read", "filesystem-write", "network"]
                .iter()
                .all(|required| plan.synthesis.capabilities.iter().any(|c| c == required))
        {
            unsupported.push(
                "synthesis python-execute requires filesystem-read, filesystem-write and network"
                    .into(),
            );
        }
        if plan.synthesis.execution_mode.as_deref() != Some("native")
            || plan.synthesis.capabilities.iter().any(|c| {
                !matches!(
                    c.as_str(),
                    "filesystem-read"
                        | "filesystem-write"
                        | "network"
                        | "memory"
                        | "github-pr"
                        | "python-execute"
                )
            })
            || (!plan.deliverables.is_empty()
                && !plan
                    .synthesis
                    .capabilities
                    .iter()
                    .any(|c| c == "filesystem-write"))
        {
            unsupported.push("single-agent execution requires native capabilities and filesystem-write for named deliverables".into());
        }
    } else if !files(
        &plan
            .synthesis
            .capabilities
            .iter()
            .filter(|c| {
                !(plan.synthesis.execution_mode.as_deref() == Some("native")
                    && c.as_str() == "memory")
            })
            .cloned()
            .collect::<Vec<_>>(),
    ) {
        unsupported.push(format!("synthesis requests capabilities {:?}; installed execution requires filesystem-read and filesystem-write, with optional governed memory in native mode", plan.synthesis.capabilities));
    }
    if plan.synthesis.max_tool_calls < 1 {
        unsupported.push("synthesis needs a positive maxToolCalls".into());
    }
    for deliverable in &plan.deliverables {
        if let Some(media_type) = deliverable.media_type.as_deref()
            && !matches!(
                media_type,
                "text/plain" | "text/markdown" | "text/csv" | "application/json"
            )
        {
            unsupported.push(format!("deliverable {} requests media type {media_type}; installed execution supports UTF-8 text/plain, text/markdown, text/csv and application/json", deliverable.name));
        }
    }
    if !unsupported.is_empty() {
        return Err(format!(
            "TypedPlanExecutionUnavailable: {}",
            unsupported.join("; ")
        ));
    }
    for role in &plan.roles {
        for (index, phase) in role.phases.iter().enumerate() {
            let inputs = if index == 0 { role.depends_on.len() } else { 1 };
            if phase.execution_mode.as_deref() == Some("native") {
                if inputs > 0 && !phase.capabilities.iter().any(|c| c == "filesystem-read") {
                    return Err(format!(
                        "TypedPlanExecutionUnavailable: role {} phase {} needs filesystem-read for its retained inputs",
                        role.name, phase.name
                    ));
                }
                continue;
            }
            let minimum = inputs + 1 + phase.required_tool_calls.len();
            if phase.max_tool_calls < minimum as i32 {
                return Err(format!(
                    "TypedPlanExecutionUnavailable: role {} phase {} needs at least {minimum} tool calls for {inputs} immutable input reads, {} required fetches and one handoff export",
                    role.name,
                    phase.name,
                    phase.required_tool_calls.len()
                ));
            }
        }
    }
    let minimum = plan.roles.len() + plan.deliverables.len();
    if plan.synthesis.execution_mode.as_deref() != Some("native")
        && plan.synthesis.max_tool_calls < minimum as i32
    {
        return Err(format!(
            "TypedPlanExecutionUnavailable: synthesis needs at least {minimum} tool calls for {} immutable role handoff reads and {} deliverable exports",
            plan.roles.len(),
            plan.deliverables.len()
        ));
    }
    Ok(())
}

/// Validate installed network and required-fetch authority independently of general
/// filesystem, fresh-context and activation constraints.
pub fn validate_reviewed_fetch_phase(phase: &ExecutionPhase) -> Result<(), String> {
    if phase.required_tool_calls.len() > 8
        || phase.required_tool_calls.len() as i32 > phase.max_tool_calls
    {
        return Err("has invalid required tool calls".into());
    }
    let mut unsupported = Vec::new();
    if phase
        .execution_mode
        .as_deref()
        .is_some_and(|mode| mode != "native")
    {
        unsupported.push("has unsupported execution mode".to_string());
    }
    let network = phase.capabilities.iter().any(|c| c == "network");
    if (!phase.required_tool_calls.is_empty() && !network)
        || (phase.execution_mode.is_none() && network && phase.required_tool_calls.is_empty())
    {
        unsupported.push("network requires reviewed exact http_fetch calls".to_string());
    }
    if phase
        .required_tool_calls
        .iter()
        .any(|call| call.name != "http_fetch")
    {
        let names = phase
            .required_tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>();
        unsupported.push(format!("requires external tool calls {names:?}, which installed reviewed execution does not support"));
    }
    if !unsupported.is_empty() {
        return Err(unsupported.join("; "));
    }
    let mut fetch_urls = std::collections::HashSet::new();
    if !phase
        .required_tool_calls
        .iter()
        .all(|call| valid_reviewed_fetch_arguments(call, &mut fetch_urls))
    {
        return Err(
            "requires unique exact public HTTPS http_fetch GET arguments and network capability"
                .into(),
        );
    }
    Ok(())
}

fn valid_reviewed_fetch_arguments<'a>(
    call: &'a ExecutionRequiredToolCall,
    fetch_urls: &mut std::collections::HashSet<&'a String>,
) -> bool {
    call.arguments.len() == 2
        && call.arguments.get("method").map(String::as_str) == Some("GET")
        && call
            .arguments
            .get("url")
            .is_some_and(|url| reviewed_fetch_url(url) && fetch_urls.insert(url))
}

fn reviewed_fetch_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let Some((host, tail)) = rest.split_once('/') else {
        return false;
    };
    let labels = host.split('.').collect::<Vec<_>>();
    let label_valid = |label: &&str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let suffix = labels.last().copied().unwrap_or("");
    value.len() <= 2048
        && host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(label_valid)
        && suffix.len() >= 2
        && suffix.bytes().all(|b| b.is_ascii_lowercase())
        && !matches!(suffix, "local" | "internal" | "localhost")
        && tail
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_~.!$&'()*+,;=:@?%-".contains(&b))
        && !tail.to_ascii_lowercase().contains("%2e")
        && tail.as_bytes().iter().enumerate().all(|(i, b)| {
            *b != b'%'
                || tail
                    .as_bytes()
                    .get(i + 1..i + 3)
                    .is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit))
        })
        && !tail
            .split('?')
            .next()
            .unwrap_or("")
            .split('/')
            .any(|p| matches!(p, "." | ".."))
}

pub fn validate_execution_plan(plan: &ExecutionPlan) -> Result<(), String> {
    if plan.schema != "kars.execution-plan/v1" {
        return Err("execution plan schema must be kars.execution-plan/v1".into());
    }
    if plan.roles.len() > 8
        || (plan.roles.is_empty() && plan.synthesis.execution_mode.as_deref() != Some("native"))
    {
        return Err("execution plan requires 1-8 roles or an explicitly native single-agent plan with no roles".into());
    }
    if plan.max_parallel < 1 || plan.max_parallel > plan.roles.len().max(1) as i32 {
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
                || phase.max_tool_calls < 0
                || (phase.execution_mode.as_deref() != Some("native") && phase.max_tool_calls > 32)
                || !valid_capabilities(&phase.capabilities)
                || phase
                    .execution_mode
                    .as_deref()
                    .is_some_and(|mode| mode != "native")
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
            let mut fetch_urls = std::collections::HashSet::new();
            for call in &phase.required_tool_calls {
                if matches!(call.name.as_str(), "foundry_web_search" | "web_search") {
                    return Err("TypedPlanExecutionUnavailable: hosted web-search is outside the finite funding contract".into());
                }
                if call.name == "http_fetch" {
                    if !phase.capabilities.iter().any(|c| c == "network")
                        || !valid_reviewed_fetch_arguments(call, &mut fetch_urls)
                    {
                        return Err(format!(
                            "role {} phase {} requires unique exact public HTTPS http_fetch GET arguments and network capability",
                            role.name, phase.name
                        ));
                    }
                    continue;
                }
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
    if !(20..=1200).contains(&plan.synthesis.objective.len()) {
        return Err(format!(
            "execution plan synthesis.objective must contain 20–1200 UTF-8 bytes; received {}. Keep the outcome concise and put detailed instructions in the package instructions.",
            plan.synthesis.objective.len()
        ));
    }
    if plan.synthesis.max_tool_calls < 0 {
        return Err("execution plan synthesis.max_tool_calls must be nonnegative".into());
    }
    if plan.synthesis.execution_mode.as_deref() != Some("native")
        && plan.synthesis.max_tool_calls > 32
    {
        return Err("execution plan synthesis.max_tool_calls exceeds the legacy limit of 32; native work must explicitly set execution_mode=\"native\" rather than reduce its required headroom".into());
    }
    if !valid_capabilities(&plan.synthesis.capabilities) {
        return Err(format!(
            "execution plan synthesis.capabilities must contain unique names from: {}",
            EXECUTION_CAPABILITIES.join(", ")
        ));
    }
    if plan
        .synthesis
        .execution_mode
        .as_deref()
        .is_some_and(|mode| mode != "native")
    {
        return Err("execution plan synthesis.execution_mode must be native when present".into());
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
