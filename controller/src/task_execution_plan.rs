// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "../../shared/execution_plan.rs"]
mod contract;
pub use contract::{ExecutionPlan, validate_activation};

#[cfg(test)]
pub(crate) use contract::tests::plan as test_plan;

pub fn schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let name = serde_json::json!({"type":"string","minLength":1,"maxLength":48,
        "pattern":"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$"});
    let objective = serde_json::json!({"type":"string","minLength":20,"maxLength":1200});
    let capabilities = serde_json::json!({"type":"array","maxItems":7,"default":[],
        "items":{"type":"string","enum":["filesystem-read","filesystem-write","shell","network","web-search","mcp","memory"]}});
    let calls = serde_json::json!({"type":"integer","format":"int32","minimum":0,"maximum":32});
    let phases = serde_json::json!({"type":"array","minItems":1,"maxItems":8,"items":{
        "type":"object","required":["maxToolCalls","name","objective"],
        "properties":{
            "name":name,"objective":objective,"capabilities":capabilities,
            "minToolCalls":{"type":"integer","format":"int32","minimum":0,"maximum":32,"default":0},
            "maxToolCalls":calls,"freshContext":{"type":"boolean","default":false},
            "requiredToolCalls":{"type":"array","maxItems":8,"items":{
                "type":"object","required":["name"],
                "properties":{
                    "name":{"type":"string","enum":["github_actions_job_logs"]},
                    "arguments":{"type":"object","maxProperties":8,"default":{},
                        "additionalProperties":{"type":"string","maxLength":4096}}
                }
            }}
        }
    }});
    let roles = serde_json::json!({"type":"array","minItems":1,"maxItems":8,"items":{
        "type":"object","required":["name","objective","phases"],
        "properties":{
            "name":name,"objective":objective,
            "dependsOn":{"type":"array","maxItems":7,"default":[],"items":name},
            "budgetTokens":{"type":"integer","format":"int64","minimum":1},
            "phases":phases
        }
    }});
    let value = serde_json::json!({
        "type":"object",
        "required":["maxParallel","roles","schema","synthesis"],
        "properties":{
            "schema":{"type":"string","enum":["kars.execution-plan/v1"]},
            "maxParallel":{"type":"integer","format":"int32","minimum":1,"maximum":8},
            "roles":roles,
            "synthesis":{"type":"object","required":["maxToolCalls","objective"],
                "properties":{"objective":objective,"capabilities":capabilities,"maxToolCalls":calls}},
            "deliverables":{"type":"array","maxItems":16,"default":[],"items":{
                "type":"object","required":["name"],"properties":{
                    "name":{"type":"string","minLength":1,"maxLength":128,"pattern":"^[a-zA-Z0-9][a-zA-Z0-9._-]*$"},
                    "mediaType":{"type":"string","minLength":1,"maxLength":128}
                }
            }}
        }
    });
    value
        .try_into()
        .expect("execution plan schema is an object")
}

#[cfg(test)]
#[path = "task_execution_plan_tests.rs"]
mod tests;
