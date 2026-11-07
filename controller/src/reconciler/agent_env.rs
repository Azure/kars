// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::runtime::RuntimeDeploymentPlan;
use serde_json::{Value, json};

pub const RESERVED_PREFIXES: &[&str] = &["AGT_", "FOUNDRY_AGENT_", "AZURE_", "IMDS_", "KARS_"];

// Linux limits each argv/env string to 32 pages; use the supported 4 KiB page floor.
pub const MAX_INSTRUCTION_BYTES: usize = 32 * 4096 - "KARS_AGENT_INSTRUCTIONS=".len() - 1;

pub fn validate_instructions(instructions: Option<&str>) -> Result<(), &'static str> {
    if let Some(instructions) = instructions {
        if instructions.contains('\0') {
            return Err("agent.instructions cannot contain NUL bytes in the runtime environment");
        }
        if instructions.len() > MAX_INSTRUCTION_BYTES {
            return Err(
                "agent.instructions exceeds the Linux 128 KiB environment-string boundary including its name and terminator; use reviewed input files for larger context",
            );
        }
    }
    Ok(())
}

pub fn merge(env: &mut Vec<Value>, plan: &RuntimeDeploymentPlan, instructions: Option<&str>) {
    if let Some(instructions) = instructions {
        env.push(json!({"name": "KARS_AGENT_INSTRUCTIONS", "value": instructions}));
    }
    let mut existing: std::collections::HashSet<String> = env
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    for (key, value) in &plan.runtime_extra_env {
        if key.is_empty()
            || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || key.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            tracing::warn!(key = %key, "extraEnv: invalid env var name, skipping");
            continue;
        }
        if RESERVED_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            tracing::warn!(key = %key, "extraEnv: key uses reserved prefix, skipping");
            continue;
        }
        if value.contains('\0') {
            tracing::warn!(key = %key, "extraEnv: value contains NUL byte, skipping");
            continue;
        }
        if !existing.insert(key.clone()) {
            tracing::debug!(key = %key, "extraEnv: overridden by reconciler, skipping");
            continue;
        }
        env.push(json!({"name": key, "value": value}));
    }
    for entry in &plan.raw_env {
        let Some(name) = entry.get("name").and_then(|n| n.as_str()) else {
            tracing::warn!("rawEnv: entry missing `name`, skipping");
            continue;
        };
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            tracing::warn!(key = %name, "rawEnv: invalid env var name, skipping");
            continue;
        }
        if RESERVED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            tracing::warn!(key = %name, "rawEnv: key uses reserved prefix, skipping");
            continue;
        }
        if !existing.insert(name.into()) {
            tracing::debug!(key = %name, "rawEnv: overridden by reconciler, skipping");
            continue;
        }
        env.push(entry.clone());
    }
}
