// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure-function compile step: `ClawEvalSpec` → binding JSON.
//!
//! Separated from the reconciler so it is unit-testable without a
//! `kube::Client`. The output JSON is consumed by the runtime path
//! (`cli/src/commands/eval.ts` + the router's existing `/openai/evals`
//! and `/evaluators` proxies in
//! `inference-router/src/routes/inference.rs`). S6 ships only the
//! producer side; S7 wires the runtime trigger and result writer.
//!
//! ## What the compiler is NOT
//!
//! - **Not** a Foundry client. Foundry calls happen at runtime
//!   through the router's Workload Identity, not from the controller.
//! - **Not** a scheduler. `schedule` flows verbatim; the trigger is
//!   wired in S7 (sandbox-side timer or router-side scheduler).
//! - **Not** a pass/fail evaluator. The threshold flows verbatim;
//!   the runtime path compares scores and writes status fields it
//!   owns.
//! - **Not** a regression actuator. `regressionAction` flows verbatim;
//!   the runtime patches the `ClawSandbox` per the operator's intent.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::claw_eval::{ClawEvalRegressionAction, ClawEvalSpec, ClawEvalThresholdOp};

/// Compile a `ClawEvalSpec` into the binding JSON the controller
/// publishes as a `ConfigMap`.
///
/// Shape (omitting `null`/empty fields):
///
/// ```json
/// {
///   "sandboxRef": { "name": "agent" },
///   "suite": "foundry-evals",
///   "evaluators": ["relevance", "coherence"],
///   "model": "gpt-4.1",
///   "dataset": { "configMapRef": { "name": "evals-cm" } },
///   "schedule": "0 */6 * * *",
///   "threshold": { "score": 0.8, "op": "Gte" },
///   "regressionAction": "Suspend",
///   "displayName": "Daily Quality Check"
/// }
/// ```
#[must_use]
pub fn compile_to_binding(spec: &ClawEvalSpec) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "sandboxRef".into(),
        json!({ "name": spec.sandbox_ref.name }),
    );
    out.insert("suite".into(), Value::String(suite_str(&spec.suite).into()));
    if !spec.evaluators.is_empty() {
        out.insert("evaluators".into(), json!(spec.evaluators));
    }
    if let Some(m) = &spec.model {
        out.insert("model".into(), Value::String(m.clone()));
    }
    if let Some(ds) = &spec.dataset {
        let mut ds_obj = serde_json::Map::new();
        if let Some(r) = &ds.config_map_ref {
            ds_obj.insert("configMapRef".into(), json!({ "name": r.name }));
        }
        if !ds.inline.is_empty() {
            ds_obj.insert("inline".into(), Value::Array(ds.inline.clone()));
        }
        if !ds_obj.is_empty() {
            out.insert("dataset".into(), Value::Object(ds_obj));
        }
    }
    if let Some(s) = &spec.schedule {
        out.insert("schedule".into(), Value::String(s.clone()));
    }
    if let Some(t) = &spec.threshold {
        out.insert(
            "threshold".into(),
            json!({
                "score": t.score,
                "op": match t.op {
                    ClawEvalThresholdOp::Gte => "Gte",
                    ClawEvalThresholdOp::Gt => "Gt",
                },
            }),
        );
    }
    let regression = spec
        .regression_action
        .clone()
        .unwrap_or(ClawEvalRegressionAction::Suspend);
    out.insert(
        "regressionAction".into(),
        Value::String(
            match regression {
                ClawEvalRegressionAction::Suspend => "Suspend",
                ClawEvalRegressionAction::None => "None",
            }
            .into(),
        ),
    );
    if let Some(d) = &spec.display_name {
        out.insert("displayName".into(), Value::String(d.clone()));
    }
    Value::Object(out)
}

fn suite_str(s: &crate::claw_eval::ClawEvalSuite) -> &'static str {
    match s {
        crate::claw_eval::ClawEvalSuite::FoundryEvals => "foundry-evals",
        crate::claw_eval::ClawEvalSuite::Promptfoo => "promptfoo",
        crate::claw_eval::ClawEvalSuite::InspectAi => "inspect-ai",
    }
}

/// Stable SHA-256 over the canonicalised compiled binding, hex-encoded
/// (first 32 chars). Used as `versionHash` for change detection.
#[must_use]
pub fn version_hash(binding: &Value) -> String {
    let bytes = serde_json::to_vec(binding).expect("serde_json::Value always serialises");
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claw_eval::{
        ClawEvalDataset, ClawEvalRegressionAction, ClawEvalSpec, ClawEvalSuite, ClawEvalThreshold,
        ClawEvalThresholdOp, SandboxRef,
    };
    use crate::mcp_server::LocalObjectRef;

    fn full_spec() -> ClawEvalSpec {
        ClawEvalSpec {
            sandbox_ref: SandboxRef {
                name: "agent-x".into(),
            },
            suite: ClawEvalSuite::FoundryEvals,
            evaluators: vec!["relevance".into(), "coherence".into()],
            model: Some("gpt-4.1".into()),
            dataset: Some(ClawEvalDataset {
                config_map_ref: Some(LocalObjectRef {
                    name: "evals-cm".into(),
                }),
                inline: vec![],
            }),
            schedule: Some("0 */6 * * *".into()),
            threshold: Some(ClawEvalThreshold {
                score: 0.8,
                op: ClawEvalThresholdOp::Gte,
            }),
            regression_action: Some(ClawEvalRegressionAction::Suspend),
            display_name: Some("Daily Quality Check".into()),
        }
    }

    #[test]
    fn compile_minimal_spec_round_trips() {
        let spec = ClawEvalSpec {
            sandbox_ref: SandboxRef {
                name: "agent".into(),
            },
            suite: ClawEvalSuite::FoundryEvals,
            ..ClawEvalSpec::default()
        };
        let binding = compile_to_binding(&spec);
        assert_eq!(binding["sandboxRef"]["name"], "agent");
        assert_eq!(binding["suite"], "foundry-evals");
        // Default regressionAction always materialises in binding.
        assert_eq!(binding["regressionAction"], "Suspend");
        assert!(binding.get("schedule").is_none());
        assert!(binding.get("evaluators").is_none());
        assert!(binding.get("dataset").is_none());
        assert!(binding.get("threshold").is_none());
    }

    #[test]
    fn compile_full_spec_round_trips() {
        let spec = full_spec();
        let binding = compile_to_binding(&spec);
        assert_eq!(binding["sandboxRef"]["name"], "agent-x");
        assert_eq!(binding["suite"], "foundry-evals");
        assert_eq!(binding["evaluators"][0], "relevance");
        assert_eq!(binding["evaluators"][1], "coherence");
        assert_eq!(binding["model"], "gpt-4.1");
        assert_eq!(binding["dataset"]["configMapRef"]["name"], "evals-cm");
        assert_eq!(binding["schedule"], "0 */6 * * *");
        assert_eq!(binding["threshold"]["score"], 0.8);
        assert_eq!(binding["threshold"]["op"], "Gte");
        assert_eq!(binding["regressionAction"], "Suspend");
        assert_eq!(binding["displayName"], "Daily Quality Check");
    }

    #[test]
    fn compile_emits_inline_dataset_when_set() {
        let spec = ClawEvalSpec {
            sandbox_ref: SandboxRef {
                name: "agent".into(),
            },
            suite: ClawEvalSuite::FoundryEvals,
            dataset: Some(ClawEvalDataset {
                config_map_ref: None,
                inline: vec![
                    json!({"input": "hello", "expected": "hi"}),
                    json!({"input": "ping", "expected": "pong"}),
                ],
            }),
            ..ClawEvalSpec::default()
        };
        let binding = compile_to_binding(&spec);
        assert!(binding["dataset"]["inline"].is_array());
        assert_eq!(binding["dataset"]["inline"].as_array().unwrap().len(), 2);
        assert!(binding["dataset"].get("configMapRef").is_none());
    }

    #[test]
    fn compile_promptfoo_and_inspect_ai_serialise_with_kebab_case() {
        let promptfoo = ClawEvalSpec {
            sandbox_ref: SandboxRef { name: "a".into() },
            suite: ClawEvalSuite::Promptfoo,
            ..ClawEvalSpec::default()
        };
        assert_eq!(compile_to_binding(&promptfoo)["suite"], "promptfoo");
        let inspect = ClawEvalSpec {
            sandbox_ref: SandboxRef { name: "a".into() },
            suite: ClawEvalSuite::InspectAi,
            ..ClawEvalSpec::default()
        };
        assert_eq!(compile_to_binding(&inspect)["suite"], "inspect-ai");
    }

    #[test]
    fn compile_threshold_op_gt_round_trips() {
        let spec = ClawEvalSpec {
            sandbox_ref: SandboxRef { name: "a".into() },
            suite: ClawEvalSuite::FoundryEvals,
            threshold: Some(ClawEvalThreshold {
                score: 0.5,
                op: ClawEvalThresholdOp::Gt,
            }),
            ..ClawEvalSpec::default()
        };
        let b = compile_to_binding(&spec);
        assert_eq!(b["threshold"]["op"], "Gt");
        assert_eq!(b["threshold"]["score"], 0.5);
    }

    #[test]
    fn compile_regression_action_none_serialises() {
        let spec = ClawEvalSpec {
            sandbox_ref: SandboxRef { name: "a".into() },
            suite: ClawEvalSuite::FoundryEvals,
            regression_action: Some(ClawEvalRegressionAction::None),
            ..ClawEvalSpec::default()
        };
        assert_eq!(compile_to_binding(&spec)["regressionAction"], "None");
    }

    #[test]
    fn compile_default_regression_action_is_suspend() {
        let spec = ClawEvalSpec {
            sandbox_ref: SandboxRef { name: "a".into() },
            suite: ClawEvalSuite::FoundryEvals,
            regression_action: None,
            ..ClawEvalSpec::default()
        };
        assert_eq!(compile_to_binding(&spec)["regressionAction"], "Suspend");
    }

    #[test]
    fn compile_is_deterministic() {
        let spec = full_spec();
        let a = compile_to_binding(&spec);
        let b = compile_to_binding(&spec);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn version_hash_changes_on_spec_change() {
        let mut a = full_spec();
        let mut b = full_spec();
        b.threshold = Some(ClawEvalThreshold {
            score: 0.95,
            op: ClawEvalThresholdOp::Gte,
        });
        let h_a = version_hash(&compile_to_binding(&a));
        let h_b = version_hash(&compile_to_binding(&b));
        assert_ne!(h_a, h_b);

        a.display_name = Some("Daily Quality Check".into());
        let h_a2 = version_hash(&compile_to_binding(&a));
        assert_eq!(h_a, h_a2);
    }

    #[test]
    fn version_hash_is_stable_across_serde_round_trip() {
        let spec = full_spec();
        let binding_a = compile_to_binding(&spec);
        let s = serde_json::to_string(&binding_a).unwrap();
        let binding_b: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(version_hash(&binding_a), version_hash(&binding_b));
    }

    #[test]
    fn version_hash_is_hex_16_bytes() {
        let h = version_hash(&compile_to_binding(&full_spec()));
        assert_eq!(h.len(), 32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
