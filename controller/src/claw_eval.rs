// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ClawEval` CRD — Phase 2 §8 entry 6 (S6).
//!
//! Per `docs/implementation-plan.md` §10.5 #6, `ClawEval` is **a binding
//! resource over Azure AI Foundry Evals + future suite adapters**. It
//! declares an evaluation workflow over a sandbox; it is **not** an
//! in-cluster eval engine and the controller does **not** run evals.
//!
//! ## Scope (S6)
//!
//! This slice ships:
//!
//! 1. The CRD schema (`ClawEval.spec`).
//! 2. The reconciler that compiles the spec to a binding JSON and
//!    publishes it as a `ConfigMap` (`claweval-{name}-binding`).
//! 3. CEL admission rules for shape invariants.
//! 4. Helm CRD with drift-checked schema.
//!
//! What this slice **does NOT** do (and why):
//!
//! - **Does NOT call Foundry directly from the controller.** The Foundry
//!   Evals API is reached from the existing CLI path
//!   (`cli/src/commands/eval.ts`) and the router proxy
//!   (`/openai/evals`, `/evaluators` in
//!   `inference-router/src/routes/inference.rs`). The controller has
//!   no Foundry credential and we explicitly do not want one.
//! - **Does NOT run a CronJob** to trigger evals. The `schedule` field
//!   is preserved verbatim in the binding; S7 wires the trigger
//!   (sandbox-side timer or router-side scheduler).
//! - **Does NOT enforce regression actions.** `regressionAction` is
//!   declared in spec; runtime path (S7) reads the binding, observes
//!   the eval result, and patches the `ClawSandbox` status / spec
//!   accordingly (e.g., `suspend=true`).
//! - **Does NOT compute pass/fail.** The threshold is preserved in the
//!   binding; the runtime path compares the observed score against it
//!   and writes `lastPass`, `lastScore`, and an `EvalsPassed` condition
//!   to status using its own field manager
//!   (`azureclaw-router/claweval`). The controller never touches those
//!   fields.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! - **CRD-derive shape**: mirrors S2/S3/S4/S5.
//! - **`LocalObjectRef`** for `bindingConfigMapRef`: re-used from
//!   [`crate::mcp_server`] — 6th semantic client.
//! - **`Condition`** vocabulary: re-used from
//!   [`crate::status::conditions`] via the reconciler module.
//! - **Foundry Evals API surface**: existing
//!   `inference-router/src/routes/inference.rs` proxy and
//!   `cli/src/commands/eval.ts` flow. **Not modified in this slice.**
//!
//! ## Field-manager split (S7 forward-compat)
//!
//! Status fields owned by the **controller** (this slice):
//! `phase`, `observedGeneration`, `conditions` (Ready / Progressing /
//! Degraded), `bindingConfigMapRef`, `versionHash`, `lastReconciledAt`.
//!
//! Status fields reserved for the **runtime** (S7):
//! `lastRunAt`, `lastScore`, `lastPass`, plus an additional
//! `EvalsPassed` condition appended via SSA with a distinct field
//! manager. Both sides use `Patch::Apply` so SSA arbitrates ownership.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp_server::LocalObjectRef;

/// `ClawEval.spec` — declares an evaluation workflow over a sandbox.
///
/// Resolution rule: a sandbox can have multiple `ClawEval` CRs (one
/// per suite or schedule). The runtime path indexes them by name.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "ClawEval",
    namespaced,
    status = "ClawEvalStatus",
    shortname = "ceval",
    printcolumn = r#"{"name":"Sandbox","type":"string","jsonPath":".spec.sandboxRef.name"}"#,
    printcolumn = r#"{"name":"Suite","type":"string","jsonPath":".spec.suite"}"#,
    printcolumn = r#"{"name":"Schedule","type":"string","jsonPath":".spec.schedule"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"LastScore","type":"string","jsonPath":".status.lastScore"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClawEvalSpec {
    /// Sandbox this eval applies to.
    pub sandbox_ref: SandboxRef,

    /// Eval suite. `foundry-evals` is the only suite with a runtime
    /// path today; `promptfoo` and `inspect-ai` are reserved for
    /// future runtime adapters and are accepted by admission so
    /// operators can pre-author manifests.
    pub suite: ClawEvalSuite,

    /// Foundry evaluator IDs (e.g., `relevance`, `coherence`,
    /// `fluency`). Required when `suite=foundry-evals`. Other suites
    /// ignore this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evaluators: Vec<String>,

    /// Model identifier the runtime path should evaluate against.
    /// Optional; runtime defaults to the sandbox's primary model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Optional dataset reference. The runtime path resolves either
    /// `configMapRef` (operator-managed JSONL) or `inline` (small
    /// inline list). Mutually exclusive — admission enforces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<ClawEvalDataset>,

    /// Cron schedule (5 or 6 space-separated tokens). When absent,
    /// the eval is manual-trigger only (`azureclaw eval <name>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,

    /// Pass/fail threshold. When absent, the runtime records the
    /// score but never marks the eval failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<ClawEvalThreshold>,

    /// Action to take when an eval run fails the threshold.
    /// Defaults to `Suspend`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_action: Option<ClawEvalRegressionAction>,

    /// Optional human-readable label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Eval suite identifier.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Default)]
pub enum ClawEvalSuite {
    /// Azure AI Foundry built-in evaluators (the only suite with a
    /// runtime path today). Routed through `/openai/evals` +
    /// `/evaluators` Foundry endpoints.
    #[default]
    #[serde(rename = "foundry-evals")]
    FoundryEvals,
    /// Reserved for a future `promptfoo` adapter.
    #[serde(rename = "promptfoo")]
    Promptfoo,
    /// Reserved for a future `inspect-ai` adapter.
    #[serde(rename = "inspect-ai")]
    InspectAi,
}

/// Dataset reference. Either a `ConfigMap` containing JSONL or an
/// inline list. Mutually exclusive — CEL enforces.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClawEvalDataset {
    /// Reference to a `ConfigMap` whose `dataset.jsonl` key contains
    /// the eval cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map_ref: Option<LocalObjectRef>,

    /// Inline list of eval cases. Each entry is a free-form JSON
    /// object the runtime path passes through to the suite. Bounded
    /// to 64 entries by CEL to keep the CR small.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "inline_dataset_schema")]
    pub inline: Vec<serde_json::Value>,
}

/// Schemars override: emit `items: { type: object,
/// x-kubernetes-preserve-unknown-fields: true }` so the resulting CRD
/// passes Kubernetes 1.25+ validation (which rejects empty `items: {}`).
fn inline_dataset_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::try_from(serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "x-kubernetes-preserve-unknown-fields": true
        }
    }))
    .expect("static inline-dataset schema")
}

/// Pass/fail threshold over the suite's primary score.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClawEvalThreshold {
    /// Numeric pass threshold, in `[0.0, 1.0]`.
    pub score: f64,

    /// Comparison operator. Defaults to `Gte`.
    #[serde(default)]
    pub op: ClawEvalThresholdOp,
}

/// Threshold comparison operator.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
pub enum ClawEvalThresholdOp {
    /// `score >= threshold` → pass.
    #[default]
    #[serde(rename = "Gte")]
    Gte,
    /// `score > threshold` → pass.
    #[serde(rename = "Gt")]
    Gt,
}

/// Action to take when an eval run fails the threshold.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Default)]
pub enum ClawEvalRegressionAction {
    /// Set `ClawSandbox.spec.suspend = true` (default). Wired by S7.
    #[default]
    #[serde(rename = "Suspend")]
    Suspend,
    /// Record the failure in conditions but take no action.
    #[serde(rename = "None")]
    None,
}

/// Reference to a sandbox by name (within the same namespace as the
/// `ClawEval` CR).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxRef {
    /// Sandbox name (`ClawSandbox.metadata.name`).
    pub name: String,
}

/// Status of a `ClawEval` reconcile.
///
/// **Field ownership** (see module docstring): the controller owns
/// `phase`, `observedGeneration`, `conditions`, `bindingConfigMapRef`,
/// `versionHash`, `lastReconciledAt`. The runtime (S7) owns
/// `lastRunAt`, `lastScore`, `lastPass`. Both sides use SSA with
/// distinct field managers so updates do not race.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClawEvalStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    /// Pointer to the binding `ConfigMap` produced by the reconciler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_config_map_ref: Option<LocalObjectRef>,

    /// Hex-encoded sha256 prefix of the compiled binding JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_hash: Option<String>,

    /// Last time the binding was compiled and pushed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reconciled_at: Option<String>,

    // ---------------------------------------------------------------
    // Runtime-owned fields (S7). Declared in schema so the API server
    // accepts them; the controller never sets these.
    // ---------------------------------------------------------------
    /// RFC3339 timestamp of the last completed eval run, written by
    /// the runtime path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,

    /// Score of the last completed eval run, written by the runtime
    /// path. Range `[0.0, 1.0]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_score: Option<f64>,

    /// Whether the last completed eval run passed `spec.threshold`,
    /// written by the runtime path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<bool>,
}
