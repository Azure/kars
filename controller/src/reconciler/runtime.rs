//! Runtime dispatch seam (S10.A2 — multi-runtime hosting).
//!
//! The reconciler used to read `spec.runtime.openclaw` directly when
//! building the agent container's `image` / `extraEnv`. That worked while
//! OpenClaw was the only runtime, but adding OpenAI Agents (S10.A3),
//! Microsoft Agent Framework (S10.A4), or BYO would have grown N parallel
//! ad-hoc lookups in the deployment builder. This module is the dispatch
//! seam: a [`RuntimeDeploymentPlan`] flattens the runtime kind down to
//! the concrete image / command / args / runtime-specific env that the
//! deployment builder consumes — adding a new runtime kind means adding
//! a producer here, not editing the deployment builder.
//!
//! Behavior in S10.A2 is **strictly equivalent** to S10.A1 for the
//! `OpenClaw` kind. All other kinds short-circuit the same
//! `AdapterMissing` path the reconciler had inline before; the seam
//! exists so adapter rollouts (A3/A4 + BYO end-to-end) only edit this
//! module.
//!
//! Two responsibilities:
//!
//! 1. [`validate_runtime_shape`] — defensive belt-and-suspenders mirror
//!    of the helm CRD CEL rules (see plan.md §S10.A1 rubber-duck #7).
//!    Defends against CEL-disabled apiservers letting through CRs where
//!    `kind` and the populated variant struct disagree.
//!
//! 2. [`build_runtime_plan`] — produces a [`RuntimeDeploymentPlan`] for
//!    runtimes whose adapter is wired in this build, or an
//!    [`AdapterMissing`](RuntimePlanError::AdapterMissing) error otherwise.

use std::collections::BTreeMap;

use crate::crd::{AgentCodeRef, ByoRuntimeConfig, OpenClawConfig, RuntimeKind, RuntimeSpec};

/// Concrete deployment intent for a single reconcile pass.
///
/// Owned by the reconciler for the duration of one reconcile; consumed by
/// the deployment builder. **Wire-format invariant**: the only field that
/// is ever rendered into `spec.containers[name=openclaw].image` is
/// [`Self::image`]; the deployment builder must not fall through to any
/// other source for the image string (plan.md §S10.A1 rubber-duck #2).
//
// Some fields are populated by the producers but not yet read by the
// deployment builder — they're the structural surface for slices A2.b
// (BYO end-to-end), A3 (OpenAIAgents), A4 (MAF). Carrying them now
// keeps the producer→builder contract stable across slices and is
// covered by unit tests on the producers themselves.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RuntimeDeploymentPlan {
    /// Stable wire-format string for the runtime kind, suitable for
    /// `status.runtimeKind`, log fields, and the `Runtime` printer column.
    /// PascalCase per the multi-runtime naming convention.
    pub kind_str: &'static str,

    /// Container image to run. Already resolved against the controller's
    /// default fallback (no further fallback in the deployment builder).
    pub image: String,

    /// Container `command:` override. `None` means honor the image's
    /// declared `ENTRYPOINT`. Only ever `Some` for runtimes that ship a
    /// fixed entrypoint (e.g. BYO with `byo.command`).
    pub command: Option<Vec<String>>,

    /// Container `args:` override. `None` means honor the image's
    /// declared `CMD`.
    pub args: Option<Vec<String>>,

    /// Runtime-specific extra env vars merged into the agent container
    /// after the controller-managed env (router URL, model name, foundry
    /// endpoint, etc.). Reserved-prefix filtering is the deployment
    /// builder's responsibility, not this module's — it sees the raw
    /// declared map.
    pub runtime_extra_env: BTreeMap<String, String>,

    /// Where the user's agent code comes from (OCI / git). `None` for
    /// `OpenClaw` (the container image *is* the agent) and `BYO` (same).
    /// Reserved for the OpenAI Agents (S10.A3) and Microsoft Agent
    /// Framework (S10.A4) producers; A2 only carries the field.
    pub agent_code: Option<AgentCodeRef>,

    /// BYO contract version (`Some` only when `kind == BYO`). The
    /// deployment builder uses this to stamp the `RuntimeReady` Condition
    /// reason; the registry-side label check is a follow-up (A2.b).
    pub byo_contract_version: Option<String>,
}

/// Errors raised when a runtime spec cannot be turned into a plan.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimePlanError {
    /// The kind is declared in the CRD but no controller-side adapter is
    /// wired in this build. Reconciler stamps `RuntimeReady=False /
    /// AdapterMissing` and refuses to deploy a Pod (the alternative —
    /// silently running the OpenClaw image — is the threat model
    /// documented in the multi-runtime audit doc).
    #[error("runtime kind `{0}` has no adapter wired in this controller build")]
    AdapterMissing(&'static str),

    /// The runtime spec passes serde but is structurally invalid. Either
    /// `kind` and the populated variant struct disagree (CEL would have
    /// caught this on a CEL-enabled apiserver) or a kind-specific field
    /// is unsupported (e.g. unknown BYO contract version).
    #[error("runtime spec shape invalid: {0}")]
    ShapeInvalid(String),
}

/// Stable PascalCase string for a [`RuntimeKind`]. Identical to the
/// inline match the reconciler had pre-S10.A2 and to the wire format
/// emitted in `status.runtimeKind`.
pub fn kind_str(kind: &RuntimeKind) -> &'static str {
    match kind {
        RuntimeKind::OpenClaw => "OpenClaw",
        RuntimeKind::OpenAIAgents => "OpenAIAgents",
        RuntimeKind::MicrosoftAgentFramework => "MicrosoftAgentFramework",
        RuntimeKind::SemanticKernel => "SemanticKernel",
        RuntimeKind::LangGraph => "LangGraph",
        RuntimeKind::Anthropic => "Anthropic",
        RuntimeKind::BYO => "BYO",
    }
}

/// Defensive guard: verify that exactly the variant struct matching
/// `kind` is set, and the others are absent. Mirrors the 7 bidirectional
/// `XValidation` rules in `deploy/helm/azureclaw/templates/crd.yaml`.
///
/// On a CEL-enabled apiserver this is a no-op (admission already rejected
/// shape-mismatched CRs). The guard exists for CEL-disabled apiservers
/// (e.g. older clusters, conformance test rigs) where a malformed CR
/// would otherwise reach the reconciler and either crash on `unwrap` or
/// — worse — pick the wrong variant silently.
pub fn validate_runtime_shape(runtime: &RuntimeSpec) -> Result<(), RuntimePlanError> {
    let kind_label = kind_str(&runtime.kind);
    // Pairs of (CEL kind name, this variant's `Option::is_some`).
    let pairs = [
        ("OpenClaw", "openclaw", runtime.openclaw.is_some()),
        (
            "OpenAIAgents",
            "openaiAgents",
            runtime.openai_agents.is_some(),
        ),
        (
            "MicrosoftAgentFramework",
            "microsoftAgentFramework",
            runtime.microsoft_agent_framework.is_some(),
        ),
        (
            "SemanticKernel",
            "semanticKernel",
            runtime.semantic_kernel.is_some(),
        ),
        ("LangGraph", "langGraph", runtime.lang_graph.is_some()),
        ("Anthropic", "anthropic", runtime.anthropic.is_some()),
        ("BYO", "byo", runtime.byo.is_some()),
    ];
    for (cel_kind, field, present) in pairs {
        let should_be_present = cel_kind == kind_label;
        if should_be_present && !present {
            return Err(RuntimePlanError::ShapeInvalid(format!(
                "spec.runtime.{field} must be set when kind={kind_label}"
            )));
        }
        if !should_be_present && present {
            return Err(RuntimePlanError::ShapeInvalid(format!(
                "spec.runtime.{field} must be absent when kind={kind_label}"
            )));
        }
    }
    Ok(())
}

/// Produce a [`RuntimeDeploymentPlan`] for the given runtime spec, or an
/// [`AdapterMissing`](RuntimePlanError::AdapterMissing) error if the kind
/// has no adapter in this build.
///
/// `default_openclaw_image` is the fallback used by the OpenClaw producer
/// when `spec.runtime.openclaw.image` is absent — same controller-context
/// default as pre-S10.A2.
pub fn build_runtime_plan(
    runtime: &RuntimeSpec,
    default_openclaw_image: &str,
) -> Result<RuntimeDeploymentPlan, RuntimePlanError> {
    validate_runtime_shape(runtime)?;
    match runtime.kind {
        RuntimeKind::OpenClaw => {
            // `validate_runtime_shape` guarantees `openclaw` is `Some`.
            let cfg = runtime.openclaw.as_ref().expect("validated above");
            Ok(plan_openclaw(cfg, default_openclaw_image))
        }
        RuntimeKind::BYO => {
            // BYO end-to-end deployment is S10.A2.b. A2 produces the plan
            // (proves the seam works) but the reconciler still
            // short-circuits to `AdapterMissing` in the call site so we
            // don't ship a half-wired BYO deployment that lacks the
            // contract verifier. Returning `AdapterMissing` here keeps
            // behavior identical to S10.A1; the producer below is exercised
            // only by unit tests in this module.
            Err(RuntimePlanError::AdapterMissing("BYO"))
        }
        RuntimeKind::OpenAIAgents => Err(RuntimePlanError::AdapterMissing("OpenAIAgents")),
        RuntimeKind::MicrosoftAgentFramework => {
            Err(RuntimePlanError::AdapterMissing("MicrosoftAgentFramework"))
        }
        RuntimeKind::SemanticKernel => Err(RuntimePlanError::AdapterMissing("SemanticKernel")),
        RuntimeKind::LangGraph => Err(RuntimePlanError::AdapterMissing("LangGraph")),
        RuntimeKind::Anthropic => Err(RuntimePlanError::AdapterMissing("Anthropic")),
    }
}

fn plan_openclaw(cfg: &OpenClawConfig, default_image: &str) -> RuntimeDeploymentPlan {
    RuntimeDeploymentPlan {
        kind_str: "OpenClaw",
        image: cfg
            .image
            .clone()
            .unwrap_or_else(|| default_image.to_string()),
        command: None,
        args: None,
        runtime_extra_env: cfg.extra_env.clone().unwrap_or_default(),
        agent_code: None,
        byo_contract_version: None,
    }
}

/// Reserved for S10.A2.b. Exercised by unit tests only; not yet plumbed
/// into the reconciler call site.
#[allow(dead_code)]
fn plan_byo(cfg: &ByoRuntimeConfig) -> RuntimeDeploymentPlan {
    let mut runtime_extra_env = BTreeMap::new();
    if let Some(env) = cfg.env.as_ref() {
        for entry in env {
            if let (Some(name), Some(value)) = (
                entry.get("name").and_then(|v| v.as_str()),
                entry.get("value").and_then(|v| v.as_str()),
            ) {
                runtime_extra_env.insert(name.to_string(), value.to_string());
            }
            // valueFrom entries are passed through to the Pod env
            // verbatim by the deployment builder; they're not flattened
            // into runtime_extra_env (would lose the secretKeyRef
            // semantic).
        }
    }
    RuntimeDeploymentPlan {
        kind_str: "BYO",
        image: cfg.image.clone(),
        command: cfg.command.clone(),
        args: cfg.args.clone(),
        runtime_extra_env,
        agent_code: None,
        byo_contract_version: Some(cfg.contract_version.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{
        AnthropicConfig, ByoRuntimeConfig, LangGraphConfig, MicrosoftAgentFrameworkConfig,
        OpenAIAgentsConfig, OpenClawConfig, SemanticKernelConfig,
    };

    fn rt_openclaw(image: Option<&str>) -> RuntimeSpec {
        RuntimeSpec {
            kind: RuntimeKind::OpenClaw,
            openclaw: Some(OpenClawConfig {
                image: image.map(String::from),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn rt_only_kind(kind: RuntimeKind) -> RuntimeSpec {
        RuntimeSpec {
            kind,
            openclaw: None,
            openai_agents: None,
            microsoft_agent_framework: None,
            semantic_kernel: None,
            lang_graph: None,
            anthropic: None,
            byo: None,
        }
    }

    // ── kind_str() — wire-format stability ───────────────────────────

    #[test]
    fn kind_str_emits_pascal_case_for_all_variants() {
        assert_eq!(kind_str(&RuntimeKind::OpenClaw), "OpenClaw");
        assert_eq!(kind_str(&RuntimeKind::OpenAIAgents), "OpenAIAgents");
        assert_eq!(
            kind_str(&RuntimeKind::MicrosoftAgentFramework),
            "MicrosoftAgentFramework"
        );
        assert_eq!(kind_str(&RuntimeKind::SemanticKernel), "SemanticKernel");
        assert_eq!(kind_str(&RuntimeKind::LangGraph), "LangGraph");
        assert_eq!(kind_str(&RuntimeKind::Anthropic), "Anthropic");
        assert_eq!(kind_str(&RuntimeKind::BYO), "BYO");
    }

    // ── validate_runtime_shape() ─────────────────────────────────────

    #[test]
    fn validate_accepts_well_formed_openclaw() {
        let rt = rt_openclaw(None);
        assert!(validate_runtime_shape(&rt).is_ok());
    }

    #[test]
    fn validate_rejects_kind_without_matching_variant_struct() {
        let mut rt = rt_only_kind(RuntimeKind::OpenAIAgents);
        // openai_agents is None, kind is OpenAIAgents → mismatch.
        let err = validate_runtime_shape(&rt).expect_err("must reject");
        assert!(
            matches!(err, RuntimePlanError::ShapeInvalid(ref m)
                if m.contains("openaiAgents") && m.contains("must be set")),
            "expected ShapeInvalid mentioning openaiAgents, got: {err}"
        );
        rt.openai_agents = Some(OpenAIAgentsConfig::default());
        assert!(validate_runtime_shape(&rt).is_ok());
    }

    #[test]
    fn validate_rejects_extra_variant_struct() {
        // kind=OpenClaw but byo is also set — multi-set is the silent-
        // wrong-variant threat the CEL rules prevent at admission, mirrored
        // here for CEL-disabled apiservers.
        let rt = RuntimeSpec {
            kind: RuntimeKind::OpenClaw,
            openclaw: Some(OpenClawConfig::default()),
            byo: Some(ByoRuntimeConfig {
                image: "x".into(),
                contract_version: "v1".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_runtime_shape(&rt).expect_err("must reject");
        assert!(
            matches!(err, RuntimePlanError::ShapeInvalid(ref m)
                if m.contains("byo") && m.contains("must be absent")),
            "expected ShapeInvalid mentioning byo absent, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_each_tier2_kind_without_struct() {
        for (kind, field) in [
            (RuntimeKind::SemanticKernel, "semanticKernel"),
            (RuntimeKind::LangGraph, "langGraph"),
            (RuntimeKind::Anthropic, "anthropic"),
        ] {
            let rt = rt_only_kind(kind);
            let err = validate_runtime_shape(&rt).expect_err("must reject");
            assert!(
                matches!(err, RuntimePlanError::ShapeInvalid(ref m) if m.contains(field)),
                "expected ShapeInvalid mentioning {field}, got: {err}"
            );
        }
    }

    #[test]
    fn validate_accepts_each_tier2_kind_with_correct_struct() {
        let pairs: Vec<(RuntimeKind, RuntimeSpec)> = vec![
            (
                RuntimeKind::SemanticKernel,
                RuntimeSpec {
                    kind: RuntimeKind::SemanticKernel,
                    openclaw: None,
                    semantic_kernel: Some(SemanticKernelConfig::default()),
                    ..Default::default()
                },
            ),
            (
                RuntimeKind::LangGraph,
                RuntimeSpec {
                    kind: RuntimeKind::LangGraph,
                    openclaw: None,
                    lang_graph: Some(LangGraphConfig::default()),
                    ..Default::default()
                },
            ),
            (
                RuntimeKind::Anthropic,
                RuntimeSpec {
                    kind: RuntimeKind::Anthropic,
                    openclaw: None,
                    anthropic: Some(AnthropicConfig::default()),
                    ..Default::default()
                },
            ),
        ];
        for (kind, rt) in pairs {
            assert!(
                validate_runtime_shape(&rt).is_ok(),
                "validate must accept well-formed {}",
                kind_str(&kind)
            );
        }
    }

    // ── build_runtime_plan() — OpenClaw path ─────────────────────────

    #[test]
    fn plan_openclaw_uses_image_from_config_when_set() {
        let rt = rt_openclaw(Some("custom.azurecr.io/openclaw:1.2.3"));
        let plan = build_runtime_plan(&rt, "default.azurecr.io/openclaw:latest").unwrap();
        assert_eq!(plan.kind_str, "OpenClaw");
        assert_eq!(plan.image, "custom.azurecr.io/openclaw:1.2.3");
        assert!(plan.command.is_none());
        assert!(plan.args.is_none());
        assert!(plan.runtime_extra_env.is_empty());
        assert!(plan.agent_code.is_none());
        assert!(plan.byo_contract_version.is_none());
    }

    #[test]
    fn plan_openclaw_falls_back_to_default_image() {
        let rt = rt_openclaw(None);
        let plan = build_runtime_plan(&rt, "default.azurecr.io/openclaw:latest").unwrap();
        assert_eq!(plan.image, "default.azurecr.io/openclaw:latest");
    }

    #[test]
    fn plan_openclaw_carries_extra_env_through() {
        let mut env = BTreeMap::new();
        env.insert("OFFLOAD_REQUEST_ID".to_string(), "req-42".to_string());
        env.insert("CUSTOM_KEY".to_string(), "value".to_string());
        let rt = RuntimeSpec {
            kind: RuntimeKind::OpenClaw,
            openclaw: Some(OpenClawConfig {
                extra_env: Some(env.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "x").unwrap();
        assert_eq!(plan.runtime_extra_env, env);
    }

    // ── build_runtime_plan() — AdapterMissing for non-OpenClaw ───────
    //
    // Behavior must be identical to S10.A1: every non-OpenClaw kind
    // surfaces `AdapterMissing(<kind>)`. BYO is a known short-circuit
    // until A2.b lands the contract-verifier path.

    #[test]
    fn plan_returns_adapter_missing_for_each_non_openclaw_kind() {
        let cases: Vec<(RuntimeKind, RuntimeSpec, &str)> = vec![
            (
                RuntimeKind::OpenAIAgents,
                RuntimeSpec {
                    kind: RuntimeKind::OpenAIAgents,
                    openclaw: None,
                    openai_agents: Some(OpenAIAgentsConfig::default()),
                    ..Default::default()
                },
                "OpenAIAgents",
            ),
            (
                RuntimeKind::MicrosoftAgentFramework,
                RuntimeSpec {
                    kind: RuntimeKind::MicrosoftAgentFramework,
                    openclaw: None,
                    microsoft_agent_framework: Some(MicrosoftAgentFrameworkConfig::default()),
                    ..Default::default()
                },
                "MicrosoftAgentFramework",
            ),
            (
                RuntimeKind::SemanticKernel,
                RuntimeSpec {
                    kind: RuntimeKind::SemanticKernel,
                    openclaw: None,
                    semantic_kernel: Some(SemanticKernelConfig::default()),
                    ..Default::default()
                },
                "SemanticKernel",
            ),
            (
                RuntimeKind::LangGraph,
                RuntimeSpec {
                    kind: RuntimeKind::LangGraph,
                    openclaw: None,
                    lang_graph: Some(LangGraphConfig::default()),
                    ..Default::default()
                },
                "LangGraph",
            ),
            (
                RuntimeKind::Anthropic,
                RuntimeSpec {
                    kind: RuntimeKind::Anthropic,
                    openclaw: None,
                    anthropic: Some(AnthropicConfig::default()),
                    ..Default::default()
                },
                "Anthropic",
            ),
            (
                RuntimeKind::BYO,
                RuntimeSpec {
                    kind: RuntimeKind::BYO,
                    openclaw: None,
                    byo: Some(ByoRuntimeConfig {
                        image: "myregistry.azurecr.io/agent:1.0".into(),
                        contract_version: "v1".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "BYO",
            ),
        ];
        for (kind, rt, expected_label) in cases {
            let err = build_runtime_plan(&rt, "default-image").expect_err("must short-circuit");
            assert_eq!(
                err,
                RuntimePlanError::AdapterMissing(expected_label),
                "kind={} should surface AdapterMissing({expected_label})",
                kind_str(&kind)
            );
        }
    }

    // ── build_runtime_plan() rejects shape-invalid input ─────────────

    #[test]
    fn plan_rejects_shape_invalid_input_before_dispatch() {
        // kind=OpenClaw with no openclaw struct — validate_runtime_shape
        // must fail BEFORE we reach plan_openclaw (which would unwrap).
        let rt = rt_only_kind(RuntimeKind::OpenClaw);
        let err = build_runtime_plan(&rt, "x").expect_err("must reject");
        assert!(
            matches!(err, RuntimePlanError::ShapeInvalid(_)),
            "expected ShapeInvalid, got: {err}"
        );
    }

    // ── plan_byo() — exercises the producer that A2.b will wire ──────

    #[test]
    fn plan_byo_carries_image_command_args_and_contract_version() {
        let cfg = ByoRuntimeConfig {
            image: "myregistry.azurecr.io/agent:1.0".into(),
            command: Some(vec!["python".into(), "-m".into(), "agent".into()]),
            args: Some(vec!["--verbose".into()]),
            env: Some(vec![serde_json::json!({"name": "FOO", "value": "bar"})]),
            contract_version: "v1".into(),
        };
        let plan = plan_byo(&cfg);
        assert_eq!(plan.kind_str, "BYO");
        assert_eq!(plan.image, "myregistry.azurecr.io/agent:1.0");
        assert_eq!(
            plan.command.as_deref(),
            Some(&["python".into(), "-m".into(), "agent".into()][..])
        );
        assert_eq!(plan.args.as_deref(), Some(&["--verbose".into()][..]));
        assert_eq!(
            plan.runtime_extra_env.get("FOO").map(String::as_str),
            Some("bar")
        );
        assert_eq!(plan.byo_contract_version.as_deref(), Some("v1"));
    }

    #[test]
    fn plan_byo_skips_value_from_env_entries() {
        // valueFrom entries (secretKeyRef, configMapKeyRef) must NOT be
        // flattened into runtime_extra_env — they have no static value
        // and the deployment builder needs to render them as raw EnvVar.
        // For A2.b the deployment builder consumes cfg.env directly for
        // those entries; this producer just makes sure the static `value`
        // path doesn't accidentally pick up the structural ones.
        let cfg = ByoRuntimeConfig {
            image: "x".into(),
            command: None,
            args: None,
            env: Some(vec![
                serde_json::json!({"name": "STATIC", "value": "ok"}),
                serde_json::json!({
                    "name": "FROM_SECRET",
                    "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}
                }),
            ]),
            contract_version: "v1".into(),
        };
        let plan = plan_byo(&cfg);
        assert_eq!(plan.runtime_extra_env.len(), 1);
        assert!(plan.runtime_extra_env.contains_key("STATIC"));
        assert!(!plan.runtime_extra_env.contains_key("FROM_SECRET"));
    }
}
