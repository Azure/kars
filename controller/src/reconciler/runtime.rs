// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// ci:loc-ok — Phase 2 multi-CRD reconciler / generated module; intentional. Tracked in plan.md §S15 follow-up.
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

use crate::crd::{
    AgentCodeRef, AnthropicConfig, ByoRuntimeConfig, LangGraphConfig, LangGraphLanguage,
    MafLanguage, MicrosoftAgentFrameworkConfig, OpenAIAgentsConfig, OpenClawConfig,
    PydanticAiConfig, RuntimeKind, RuntimeSpec,
};

/// Default container image for the OpenAI Agents Python runtime
/// (S10.A3). Stays `:latest` per the repo-wide image-tag rule (see
/// `.github/copilot-instructions.md` and `plan.md` §image-tag-rule).
/// Operators wanting to pin a specific tag can override at the
/// controller deployment level via the `OPENAI_AGENTS_RUNTIME_IMAGE`
/// env var (read by [`openai_agents_default_image`]).
pub const DEFAULT_OPENAI_AGENTS_IMAGE: &str =
    "karsacr.azurecr.io/kars-runtime-openai-agents:latest";

/// Resolve the OpenAI Agents adapter image, honouring an operator
/// override via `OPENAI_AGENTS_RUNTIME_IMAGE`. Falls back to
/// [`DEFAULT_OPENAI_AGENTS_IMAGE`].
pub fn openai_agents_default_image() -> String {
    std::env::var("OPENAI_AGENTS_RUNTIME_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_AGENTS_IMAGE.to_string())
}

/// Default container image for the Microsoft Agent Framework Python
/// runtime (S10.A4). MAF is the strategically prioritized
/// Microsoft-aligned runtime — it integrates natively with AGT and is
/// the unified successor to AutoGen v0.4. Stays `:latest`; operators
/// pin via the `MAF_RUNTIME_IMAGE` env var.
///
/// MAF .NET path is **deferred to Phase 3** (blocked on
/// AgentMesh.Sdk .NET availability — see
/// `docs/internal/agt-upstream-asks.md` §3). Until then, requesting
/// `language: dotnet` falls through to a `ShapeInvalid` error stamped
/// as `Degraded / SpecInvalid` in the Conditions chain.
pub const DEFAULT_MAF_PYTHON_IMAGE: &str = "karsacr.azurecr.io/kars-runtime-maf-python:latest";

/// Resolve the MAF Python adapter image, honouring an operator
/// override via `MAF_RUNTIME_IMAGE`. Falls back to
/// [`DEFAULT_MAF_PYTHON_IMAGE`].
pub fn maf_python_default_image() -> String {
    std::env::var("MAF_RUNTIME_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MAF_PYTHON_IMAGE.to_string())
}

/// Default container image for the Anthropic Claude Agent SDK Python
/// runtime (Phase H#1). Stays `:latest`; operators pin via the
/// `ANTHROPIC_RUNTIME_IMAGE` env var.
///
/// Why a dedicated adapter (vs BYO): pins ANTHROPIC_BASE_URL to the
/// router sidecar so the SDK cannot reach api.anthropic.com directly,
/// substitutes ANTHROPIC_API_KEY with a sentinel (router brokers the
/// real credential on egress), and inherits the standard AAD broker /
/// OTel / AgentMesh wiring.
pub const DEFAULT_ANTHROPIC_IMAGE: &str = "karsacr.azurecr.io/kars-runtime-anthropic:latest";

/// Resolve the Anthropic adapter image, honouring an operator
/// override via `ANTHROPIC_RUNTIME_IMAGE`. Falls back to
/// [`DEFAULT_ANTHROPIC_IMAGE`].
pub fn anthropic_default_image() -> String {
    std::env::var("ANTHROPIC_RUNTIME_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ANTHROPIC_IMAGE.to_string())
}

/// Default container image for the LangGraph Python runtime
/// (Phase H#2). LangGraph is the most-deployed OSS agent stack
/// (LangChain ecosystem). Stays `:latest`; operators pin via the
/// `LANGGRAPH_RUNTIME_IMAGE` env var.
///
/// Why a dedicated adapter (vs BYO): LangGraph relies on LangChain
/// model factories that read `OPENAI_BASE_URL` (and provider-specific
/// equivalents) at construction time. The adapter pins those to the
/// router sidecar and wires AGT/OTel/AAD broker on bootstrap.
pub const DEFAULT_LANGGRAPH_PYTHON_IMAGE: &str = "karsacr.azurecr.io/kars-runtime-langgraph:latest";

/// Default container image for the LangGraph **TypeScript** runtime
/// (LangGraph.js). Mirrors the Python adapter via a Node 22 sandbox
/// image. Operators pin via `LANGGRAPH_TS_RUNTIME_IMAGE`.
pub const DEFAULT_LANGGRAPH_TS_IMAGE: &str = "karsacr.azurecr.io/kars-runtime-langgraph-ts:latest";

/// Resolve the LangGraph Python adapter image, honouring an operator
/// override via `LANGGRAPH_RUNTIME_IMAGE`. Falls back to
/// [`DEFAULT_LANGGRAPH_PYTHON_IMAGE`].
pub fn langgraph_default_image() -> String {
    std::env::var("LANGGRAPH_RUNTIME_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LANGGRAPH_PYTHON_IMAGE.to_string())
}

/// Resolve the LangGraph TypeScript adapter image, honouring
/// `LANGGRAPH_TS_RUNTIME_IMAGE`. Falls back to
/// [`DEFAULT_LANGGRAPH_TS_IMAGE`].
pub fn langgraph_ts_default_image() -> String {
    std::env::var("LANGGRAPH_TS_RUNTIME_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LANGGRAPH_TS_IMAGE.to_string())
}

/// Default container image for the Pydantic-AI Python runtime
/// (Phase H#3). Stays `:latest`; operators pin via the
/// `PYDANTIC_AI_RUNTIME_IMAGE` env var.
///
/// Why a dedicated adapter (vs BYO): Pydantic-AI is provider-agnostic
/// (the same agent can call OpenAI, Anthropic, Gemini, ...). The
/// adapter pins each known provider base URL to the router sidecar
/// at bootstrap and substitutes API keys with router-managed
/// sentinels — same defence pattern as LangGraph.
pub const DEFAULT_PYDANTIC_AI_IMAGE: &str = "karsacr.azurecr.io/kars-runtime-pydantic-ai:latest";

/// Resolve the Pydantic-AI adapter image, honouring an operator
/// override via `PYDANTIC_AI_RUNTIME_IMAGE`. Falls back to
/// [`DEFAULT_PYDANTIC_AI_IMAGE`].
pub fn pydantic_ai_default_image() -> String {
    std::env::var("PYDANTIC_AI_RUNTIME_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PYDANTIC_AI_IMAGE.to_string())
}

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

    /// Raw EnvVar entries that must be rendered verbatim (used by BYO
    /// when `byo.env[*].valueFrom` carries `secretKeyRef` /
    /// `configMapKeyRef` / `fieldRef` — flattening would lose semantics).
    /// Reserved-prefix / NUL / dup filtering is the deployment builder's
    /// responsibility, applied to the `name` field. Empty for OpenClaw.
    pub raw_env: Vec<serde_json::Value>,

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
        RuntimeKind::PydanticAi => "PydanticAi",
        RuntimeKind::BYO => "BYO",
    }
}

/// Defensive guard: verify that exactly the variant struct matching
/// `kind` is set, and the others are absent. Mirrors the 7 bidirectional
/// `XValidation` rules in `deploy/helm/kars/templates/crd.yaml`.
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
        ("PydanticAi", "pydanticAi", runtime.pydantic_ai.is_some()),
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
            // S10.A2.b: BYO is wired end-to-end. The producer emits
            // `image` / `command` / `args` / `runtime_extra_env` /
            // `raw_env` / `byo_contract_version`; the deployment builder
            // branches the agent container shape on `kind == BYO`
            // (different container name, no port assumption, no
            // OpenClaw-specific env, no admin-token mount).
            let cfg = runtime.byo.as_ref().expect("validated above");
            Ok(plan_byo(cfg))
        }
        RuntimeKind::OpenAIAgents => {
            // S10.A3: OpenAI Agents Python adapter wired end-to-end at
            // the controller level. Adapter image bundles Python 3.12
            // + `openai-agents` + a stock entrypoint that points the
            // `openai` SDK at the inference router and exposes the
            // platform MCP server URL via env. User agent code lands
            // via `agentCode.oci` / `agentCode.git` (mounted to
            // /sandbox/agent — same convention as BYO).
            //
            // **Class B (mesh / spawn / handoff) is NOT yet exposed**
            // — blocked on AgentMesh-Python availability (see
            // `docs/internal/agt-upstream-asks.md` §3). Foundry-shim
            // affordances are reachable via `/platform/mcp` (S10.B).
            let cfg = runtime.openai_agents.as_ref().expect("validated above");
            Ok(plan_openai_agents(cfg))
        }
        RuntimeKind::MicrosoftAgentFramework => {
            // S10.A4: MAF is the strategically prioritized Microsoft-aligned
            // runtime (unified successor to AutoGen v0.4; first-party
            // AGT integration). MAF .NET is **deferred to Phase 3**
            // pending AgentMesh.Sdk .NET availability — see
            // `docs/internal/agt-upstream-asks.md` §3. Until then,
            // `language: dotnet` triggers `ShapeInvalid` so the
            // operator gets a clear error rather than a silently
            // mis-imaged pod.
            //
            // **Class B (mesh / spawn / handoff) is NOT yet exposed**
            // — same blocker as S10.A3 (AgentMesh-Python). Foundry-
            // shim affordances are reachable via `/platform/mcp`
            // (S10.B).
            let cfg = runtime
                .microsoft_agent_framework
                .as_ref()
                .expect("validated above");
            plan_microsoft_agent_framework(cfg)
        }
        RuntimeKind::SemanticKernel => Err(RuntimePlanError::AdapterMissing("SemanticKernel")),
        RuntimeKind::LangGraph => {
            // LangGraph adapters wired end-to-end for both Python
            // (`runtimes/langgraph/`) and TypeScript / Node 22
            // (`runtimes/langgraph-ts/`). `plan_langgraph` matches on
            // `language` and selects the matching image.
            let cfg = runtime.lang_graph.as_ref().expect("validated above");
            plan_langgraph(cfg)
        }
        RuntimeKind::Anthropic => {
            // Phase H#1: Anthropic Claude Agent SDK adapter wired
            // end-to-end. Adapter image bundles Python 3.12 +
            // `anthropic` SDK + `claude-agent-sdk` + a stock
            // entrypoint that pins ANTHROPIC_BASE_URL to the router
            // sidecar and replaces ANTHROPIC_API_KEY with a sentinel.
            // Foundry tools reachable via the platform MCP server at
            // `/platform/mcp` (Claude SDK supports MCP natively via
            // `mcp_servers=[...]`).
            let cfg = runtime.anthropic.as_ref().expect("validated above");
            Ok(plan_anthropic(cfg))
        }
        RuntimeKind::PydanticAi => {
            // Phase H#3: Pydantic-AI adapter wired end-to-end.
            // Provider-agnostic — adapter pins multiple LLM provider
            // base URLs to the router sidecar at bootstrap.
            let cfg = runtime.pydantic_ai.as_ref().expect("validated above");
            Ok(plan_pydantic_ai(cfg))
        }
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
        raw_env: Vec::new(),
        agent_code: None,
        byo_contract_version: None,
    }
}

fn plan_byo(cfg: &ByoRuntimeConfig) -> RuntimeDeploymentPlan {
    let mut runtime_extra_env = BTreeMap::new();
    let mut raw_env: Vec<serde_json::Value> = Vec::new();
    if let Some(env) = cfg.env.as_ref() {
        for entry in env {
            if let (Some(name), Some(value)) = (
                entry.get("name").and_then(|v| v.as_str()),
                entry.get("value").and_then(|v| v.as_str()),
            ) {
                runtime_extra_env.insert(name.to_string(), value.to_string());
            } else if entry.get("name").and_then(|v| v.as_str()).is_some() {
                // Structural entry (e.g. `valueFrom`). Pass through to
                // the deployment builder verbatim — flattening would
                // lose the secretKeyRef / configMapKeyRef / fieldRef
                // semantic. Reserved-prefix / dup filtering is applied
                // by the builder against the `name` field.
                raw_env.push(entry.clone());
            }
            // Drop entries with no `name`: malformed input. (Helm CEL
            // already rejects these at admission; this is defensive.)
        }
    }
    RuntimeDeploymentPlan {
        kind_str: "BYO",
        image: cfg.image.clone(),
        command: cfg.command.clone(),
        args: cfg.args.clone(),
        runtime_extra_env,
        raw_env,
        agent_code: None,
        byo_contract_version: Some(cfg.contract_version.clone()),
    }
}

/// S10.A3 producer for the OpenAI Agents Python runtime.
///
/// The adapter image (`sandbox-images/openai-agents/`) bundles Python
/// 3.12 + `openai-agents` + an entrypoint that:
///
/// 1. Sets `OPENAI_BASE_URL` to the inference router's `/openai/v1`
///    proxy. The user's agent uses the standard `openai` SDK with no
///    Azure-specific code; the router handles AAD / IMDS upstream auth
///    and InferencePolicy enforcement.
/// 2. Sets `KARS_PLATFORM_MCP_URL` to the platform MCP server
///    (S10.B). Adapters / user code that want Foundry-shim affordances
///    (web search, code execute, memory store, …) point their MCP
///    client at this URL.
/// 3. Exec's the user agent code from `/sandbox/agent/` (mounted via
///    `agentCode.oci` / `agentCode.git`).
///
/// Class B mesh / spawn / handoff tools are **not** exposed in this
/// slice — blocked on AgentMesh-Python availability (see
/// `docs/internal/agt-upstream-asks.md` §3). The adapter ships the
/// Foundry-shim discovery surface only.
fn plan_openai_agents(cfg: &OpenAIAgentsConfig) -> RuntimeDeploymentPlan {
    // Adapter image is controller-managed. `OpenAIAgentsConfig` does
    // not expose an `image` field today (deliberate: keeping the
    // adapter version flat across a controller release reduces blast
    // radius for AAD / OpenAI-SDK breaking changes). Operators
    // override at the controller deployment level via env.
    let image = openai_agents_default_image();

    // Inject **adapter-defaulted** env first; user `extraEnv` merges
    // on top so a sandbox author can pin a stricter
    // `OPENAI_AGENTS_LOG_LEVEL`, etc. Reserved-prefix filtering of
    // user `extraEnv` happens in the deployment builder, not here —
    // mirroring `plan_openclaw` semantics. NB: we deliberately do NOT
    // use the `KARS_*` prefix here — that prefix is reserved and
    // would be stripped by the deployment builder. `RUNTIME_*` is the
    // free-to-use convention for runtime-adapter-visible settings.
    let mut runtime_extra_env = BTreeMap::new();
    if let Some(v) = cfg.python_version.as_ref() {
        runtime_extra_env.insert("RUNTIME_PYTHON_VERSION".to_string(), v.clone());
    }
    if let Some(user_env) = cfg.extra_env.as_ref() {
        for (k, v) in user_env {
            runtime_extra_env.insert(k.clone(), v.clone());
        }
    }

    // `entrypoint` lets the user pin a non-default agent launcher
    // (e.g. `["python", "-m", "myteam.agents.researcher"]`). Default
    // — `None` — honours the adapter image's stock entrypoint, which
    // execs `/sandbox/agent/main.py` if present.
    let command = cfg.entrypoint.clone();

    RuntimeDeploymentPlan {
        kind_str: "OpenAIAgents",
        image,
        command,
        args: None,
        runtime_extra_env,
        raw_env: Vec::new(),
        agent_code: cfg.agent_code.clone(),
        byo_contract_version: None,
    }
}

/// Producer for [`RuntimeKind::MicrosoftAgentFramework`] (S10.A4).
///
/// MAF Python is wired in this build. .NET MAF is `[GAP-V1]`: blocked
/// upstream on the absence of an `AgentMesh` client class in the
/// `Microsoft.AgentGovernance` .NET package (it ships trust /
/// identity / policy / audit only). The CRD enum has been narrowed
/// to `["python"]` for v1.0; .NET will be re-introduced in v1.1 once
/// either the upstream package adds a relay client or we ship our
/// own .NET HTTP/WS bridge. See `docs/internal/agt-upstream-asks.md`.
fn plan_microsoft_agent_framework(
    cfg: &MicrosoftAgentFrameworkConfig,
) -> Result<RuntimeDeploymentPlan, RuntimePlanError> {
    let lang = cfg.language.clone().unwrap_or_default();
    let (image, lang_str) = match lang {
        MafLanguage::Python => (maf_python_default_image(), "python"),
    };

    let mut runtime_extra_env: BTreeMap<String, String> = BTreeMap::new();
    runtime_extra_env.insert("RUNTIME_MAF_LANGUAGE".to_string(), lang_str.to_string());
    if let Some(user_env) = cfg.extra_env.as_ref() {
        for (k, v) in user_env {
            runtime_extra_env.insert(k.clone(), v.clone());
        }
    }

    let command = cfg.entrypoint.clone();

    Ok(RuntimeDeploymentPlan {
        kind_str: "MicrosoftAgentFramework",
        image,
        command,
        args: None,
        runtime_extra_env,
        raw_env: Vec::new(),
        agent_code: cfg.agent_code.clone(),
        byo_contract_version: None,
    })
}

/// Producer for [`RuntimeKind::Anthropic`] (Phase H#1).
///
/// The Anthropic Claude Agent SDK is currently Python-first. The
/// adapter pins `ANTHROPIC_BASE_URL` to the router sidecar and sets
/// `ANTHROPIC_API_KEY` to a sentinel — the router brokers the real
/// credential on egress so no Anthropic key ever lives inside the
/// sandbox pod. Foundry tools are reachable via the platform MCP
/// server at `/platform/mcp` (Claude SDK supports MCP natively via
/// `mcp_servers=[...]`).
fn plan_anthropic(cfg: &AnthropicConfig) -> RuntimeDeploymentPlan {
    let image = anthropic_default_image();

    // Same merge contract as `plan_openai_agents`: producer-supplied
    // defaults first, user `extra_env` on top. Reserved-prefix
    // filtering happens in the deployment builder; keep producer
    // defaults on non-reserved (`RUNTIME_*`) keys.
    let mut runtime_extra_env: BTreeMap<String, String> = BTreeMap::new();
    if let Some(v) = cfg.python_version.as_ref() {
        runtime_extra_env.insert("RUNTIME_PYTHON_VERSION".to_string(), v.clone());
    }
    if let Some(user_env) = cfg.extra_env.as_ref() {
        for (k, v) in user_env {
            runtime_extra_env.insert(k.clone(), v.clone());
        }
    }

    let command = cfg.entrypoint.clone();

    RuntimeDeploymentPlan {
        kind_str: "Anthropic",
        image,
        command,
        args: None,
        runtime_extra_env,
        raw_env: Vec::new(),
        agent_code: cfg.agent_code.clone(),
        byo_contract_version: None,
    }
}

/// Producer for [`RuntimeKind::LangGraph`] (Phase H#2).
///
/// LangGraph (LangChain's graph-orchestration agent framework) ships
/// in Python and TypeScript. This producer wires the **Python**
/// flavour. TypeScript is **deferred** — `language: typescript` is
/// rejected with `ShapeInvalid` so the operator gets a clear error
/// rather than a silently mis-imaged pod (mirrors the MAF .NET gate).
///
/// The adapter image pins the LangChain provider base URLs
/// (`OPENAI_BASE_URL`, etc.) to the router sidecar at bootstrap time
/// so model calls cannot egress directly. No third-party API key
/// ever lives in the sandbox pod — the router brokers credentials on
/// egress.
fn plan_langgraph(cfg: &LangGraphConfig) -> Result<RuntimeDeploymentPlan, RuntimePlanError> {
    let lang = cfg.language.clone().unwrap_or_default();
    let (image, lang_str) = match lang {
        LangGraphLanguage::Python => (langgraph_default_image(), "python"),
        LangGraphLanguage::Typescript => (langgraph_ts_default_image(), "typescript"),
    };

    let mut runtime_extra_env: BTreeMap<String, String> = BTreeMap::new();
    runtime_extra_env.insert(
        "RUNTIME_LANGGRAPH_LANGUAGE".to_string(),
        lang_str.to_string(),
    );
    if let Some(user_env) = cfg.extra_env.as_ref() {
        for (k, v) in user_env {
            runtime_extra_env.insert(k.clone(), v.clone());
        }
    }

    let command = cfg.entrypoint.clone();

    Ok(RuntimeDeploymentPlan {
        kind_str: "LangGraph",
        image,
        command,
        args: None,
        runtime_extra_env,
        raw_env: Vec::new(),
        agent_code: cfg.agent_code.clone(),
        byo_contract_version: None,
    })
}

/// Producer for [`RuntimeKind::PydanticAi`] (Phase H#3).
///
/// [Pydantic-AI](https://ai.pydantic.dev/) is the type-safe Python
/// agent framework from the Pydantic team. Provider-agnostic: a
/// single `Agent` definition can target OpenAI, Anthropic, Gemini,
/// or Azure OpenAI. The adapter pins each known provider base URL
/// to the router sidecar at bootstrap and substitutes API keys with
/// router-managed sentinels — same defence pattern as LangGraph.
fn plan_pydantic_ai(cfg: &PydanticAiConfig) -> RuntimeDeploymentPlan {
    let image = pydantic_ai_default_image();

    let mut runtime_extra_env: BTreeMap<String, String> = BTreeMap::new();
    if let Some(v) = cfg.python_version.as_ref() {
        runtime_extra_env.insert("RUNTIME_PYTHON_VERSION".to_string(), v.clone());
    }
    if let Some(user_env) = cfg.extra_env.as_ref() {
        for (k, v) in user_env {
            runtime_extra_env.insert(k.clone(), v.clone());
        }
    }

    let command = cfg.entrypoint.clone();

    RuntimeDeploymentPlan {
        kind_str: "PydanticAi",
        image,
        command,
        args: None,
        runtime_extra_env,
        raw_env: Vec::new(),
        agent_code: cfg.agent_code.clone(),
        byo_contract_version: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{
        AgentCodeRef, AnthropicConfig, ByoRuntimeConfig, LangGraphConfig, LangGraphLanguage,
        MafLanguage, MicrosoftAgentFrameworkConfig, OciAgentCode, OpenAIAgentsConfig,
        OpenClawConfig, PydanticAiConfig, SemanticKernelConfig,
    };
    use std::sync::Mutex;

    /// Serialises tests that mutate process-wide image-override env vars
    /// (e.g. `OPENAI_AGENTS_RUNTIME_IMAGE`). Without this guard, the
    /// default multi-threaded test harness lets one test's `set_var`
    /// leak into another test's `*_default_image()` call, producing
    /// flaky failures of the shape:
    ///   left: "myacr.azurecr.io/openai-agents:pinned"
    ///   right: "karsacr.azurecr.io/kars-runtime-openai-agents:latest"
    /// All env-mutating tests in this module take the lock at the top.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
            pydantic_ai: None,
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
        assert_eq!(kind_str(&RuntimeKind::PydanticAi), "PydanticAi");
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
            (RuntimeKind::PydanticAi, "pydanticAi"),
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
            (
                RuntimeKind::PydanticAi,
                RuntimeSpec {
                    kind: RuntimeKind::PydanticAi,
                    openclaw: None,
                    pydantic_ai: Some(PydanticAiConfig::default()),
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

    /// LangGraph wired in Phase H#2 (joining Anthropic/OpenAIAgents/MAF/BYO).
    /// Only SemanticKernel remains as Tier-2 placeholder.
    #[test]
    fn plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind() {
        let cases: Vec<(RuntimeKind, RuntimeSpec, &str)> = vec![(
            RuntimeKind::SemanticKernel,
            RuntimeSpec {
                kind: RuntimeKind::SemanticKernel,
                openclaw: None,
                semantic_kernel: Some(SemanticKernelConfig::default()),
                ..Default::default()
            },
            "SemanticKernel",
        )];
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

    // ── Anthropic adapter (Phase H#1) ────────────────────────────────

    #[test]
    fn anthropic_default_image_falls_back_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: this test does not run in parallel with env writers
        // (no other test sets ANTHROPIC_RUNTIME_IMAGE).
        unsafe {
            std::env::remove_var("ANTHROPIC_RUNTIME_IMAGE");
        }
        assert_eq!(anthropic_default_image(), DEFAULT_ANTHROPIC_IMAGE);
    }

    #[test]
    fn plan_anthropic_emits_anthropic_kind_str_and_default_image() {
        let rt = RuntimeSpec {
            kind: RuntimeKind::Anthropic,
            openclaw: None,
            anthropic: Some(AnthropicConfig::default()),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("anthropic plan ok");
        assert_eq!(plan.kind_str, "Anthropic");
        assert!(plan.image.contains("kars-runtime-anthropic"));
        // No reserved KARS_/ANTHROPIC_ leak from the producer —
        // the deployment builder owns those.
        for k in plan.runtime_extra_env.keys() {
            assert!(
                !k.starts_with("ANTHROPIC_"),
                "producer must not emit ANTHROPIC_* keys: {k}"
            );
            assert!(
                !k.starts_with("KARS_"),
                "producer must not emit KARS_* keys: {k}"
            );
        }
    }

    #[test]
    fn plan_anthropic_threads_python_version_and_user_extra_env() {
        let mut user_env = BTreeMap::new();
        user_env.insert("MY_FLAG".to_string(), "on".to_string());
        let rt = RuntimeSpec {
            kind: RuntimeKind::Anthropic,
            openclaw: None,
            anthropic: Some(AnthropicConfig {
                python_version: Some("3.13".into()),
                extra_env: Some(user_env),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("ok");
        assert_eq!(
            plan.runtime_extra_env.get("RUNTIME_PYTHON_VERSION"),
            Some(&"3.13".to_string())
        );
        assert_eq!(
            plan.runtime_extra_env.get("MY_FLAG"),
            Some(&"on".to_string())
        );
    }

    #[test]
    fn plan_anthropic_user_extra_env_overrides_producer_default() {
        // User explicitly pinning RUNTIME_PYTHON_VERSION via extra_env
        // wins over the python_version field. Mirrors the merge order
        // in `plan_openai_agents`.
        let mut user_env = BTreeMap::new();
        user_env.insert("RUNTIME_PYTHON_VERSION".to_string(), "3.11".into());
        let rt = RuntimeSpec {
            kind: RuntimeKind::Anthropic,
            openclaw: None,
            anthropic: Some(AnthropicConfig {
                python_version: Some("3.13".into()),
                extra_env: Some(user_env),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("ok");
        assert_eq!(
            plan.runtime_extra_env.get("RUNTIME_PYTHON_VERSION"),
            Some(&"3.11".to_string())
        );
    }

    // ── LangGraph adapter (Phase H#2) ────────────────────────────────

    #[test]
    fn langgraph_default_image_falls_back_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("LANGGRAPH_RUNTIME_IMAGE");
        }
        assert_eq!(langgraph_default_image(), DEFAULT_LANGGRAPH_PYTHON_IMAGE);
    }

    #[test]
    fn plan_langgraph_python_default_succeeds() {
        let rt = RuntimeSpec {
            kind: RuntimeKind::LangGraph,
            openclaw: None,
            lang_graph: Some(LangGraphConfig::default()),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("default (Python) must plan");
        assert_eq!(plan.kind_str, "LangGraph");
        assert!(plan.image.contains("kars-runtime-langgraph"));
        assert_eq!(
            plan.runtime_extra_env.get("RUNTIME_LANGGRAPH_LANGUAGE"),
            Some(&"python".to_string())
        );
    }

    #[test]
    fn plan_langgraph_explicit_python_succeeds() {
        let rt = RuntimeSpec {
            kind: RuntimeKind::LangGraph,
            openclaw: None,
            lang_graph: Some(LangGraphConfig {
                language: Some(LangGraphLanguage::Python),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("explicit Python must plan");
        assert_eq!(plan.kind_str, "LangGraph");
    }

    #[test]
    fn plan_langgraph_typescript_dispatches_to_ts_image() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("LANGGRAPH_TS_RUNTIME_IMAGE");
        }
        let rt = RuntimeSpec {
            kind: RuntimeKind::LangGraph,
            openclaw: None,
            lang_graph: Some(LangGraphConfig {
                language: Some(LangGraphLanguage::Typescript),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan =
            build_runtime_plan(&rt, "ignored").expect("typescript must dispatch to ts image");
        assert_eq!(plan.kind_str, "LangGraph");
        assert_eq!(plan.image, DEFAULT_LANGGRAPH_TS_IMAGE);
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_LANGGRAPH_LANGUAGE")
                .map(|s| s.as_str()),
            Some("typescript")
        );
    }

    #[test]
    fn plan_langgraph_user_extra_env_overrides_runtime_default() {
        // User can override RUNTIME_LANGGRAPH_LANGUAGE (no-op effect
        // since image is python-only, but the merge order must match
        // the contract of every other producer).
        let mut user_env = BTreeMap::new();
        user_env.insert("MY_FLAG".to_string(), "on".to_string());
        let rt = RuntimeSpec {
            kind: RuntimeKind::LangGraph,
            openclaw: None,
            lang_graph: Some(LangGraphConfig {
                extra_env: Some(user_env),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("ok");
        assert_eq!(
            plan.runtime_extra_env.get("MY_FLAG"),
            Some(&"on".to_string())
        );
        // Producer default still present
        assert_eq!(
            plan.runtime_extra_env.get("RUNTIME_LANGGRAPH_LANGUAGE"),
            Some(&"python".to_string())
        );
    }

    // ── Pydantic-AI adapter (Phase H#3) ──────────────────────────────

    #[test]
    fn pydantic_ai_default_image_falls_back_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("PYDANTIC_AI_RUNTIME_IMAGE");
        }
        assert_eq!(pydantic_ai_default_image(), DEFAULT_PYDANTIC_AI_IMAGE);
    }

    #[test]
    fn plan_pydantic_ai_emits_pydanticai_kind_str_and_default_image() {
        let rt = RuntimeSpec {
            kind: RuntimeKind::PydanticAi,
            openclaw: None,
            pydantic_ai: Some(PydanticAiConfig::default()),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("pydantic_ai plan ok");
        assert_eq!(plan.kind_str, "PydanticAi");
        assert!(plan.image.contains("kars-runtime-pydantic-ai"));
        for k in plan.runtime_extra_env.keys() {
            assert!(
                !k.starts_with("KARS_"),
                "producer must not emit KARS_* keys: {k}"
            );
        }
    }

    #[test]
    fn plan_pydantic_ai_threads_python_version_and_user_extra_env() {
        let mut user_env = BTreeMap::new();
        user_env.insert("MY_FLAG".to_string(), "on".to_string());
        let rt = RuntimeSpec {
            kind: RuntimeKind::PydanticAi,
            openclaw: None,
            pydantic_ai: Some(PydanticAiConfig {
                python_version: Some("3.13".into()),
                extra_env: Some(user_env),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("ok");
        assert_eq!(
            plan.runtime_extra_env.get("RUNTIME_PYTHON_VERSION"),
            Some(&"3.13".to_string())
        );
        assert_eq!(
            plan.runtime_extra_env.get("MY_FLAG"),
            Some(&"on".to_string())
        );
    }

    #[test]
    fn plan_pydantic_ai_user_extra_env_overrides_producer_default() {
        let mut user_env = BTreeMap::new();
        user_env.insert("RUNTIME_PYTHON_VERSION".to_string(), "3.11".into());
        let rt = RuntimeSpec {
            kind: RuntimeKind::PydanticAi,
            openclaw: None,
            pydantic_ai: Some(PydanticAiConfig {
                python_version: Some("3.13".into()),
                extra_env: Some(user_env),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored").expect("ok");
        assert_eq!(
            plan.runtime_extra_env.get("RUNTIME_PYTHON_VERSION"),
            Some(&"3.11".to_string())
        );
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
        // S10.A2.b: structural entries are now passed through verbatim
        // in raw_env so the deployment builder can render them as raw
        // K8s EnvVar JSON.
        assert_eq!(plan.raw_env.len(), 1);
        assert_eq!(
            plan.raw_env[0].get("name").and_then(|v| v.as_str()),
            Some("FROM_SECRET")
        );
        assert!(plan.raw_env[0].get("valueFrom").is_some());
    }

    /// S10.A2.b: BYO with a well-formed config produces a plan, not
    /// AdapterMissing. Image / command / args / contract_version /
    /// raw_env must all flow through to the plan.
    #[test]
    fn build_runtime_plan_dispatches_byo_to_producer() {
        let rt = RuntimeSpec {
            kind: RuntimeKind::BYO,
            openclaw: None,
            byo: Some(ByoRuntimeConfig {
                image: "myregistry.azurecr.io/agent:1.0".into(),
                command: Some(vec!["python".into()]),
                args: Some(vec!["-m".into(), "agent".into()]),
                env: Some(vec![
                    serde_json::json!({"name": "MODE", "value": "prod"}),
                    serde_json::json!({
                        "name": "API_KEY",
                        "valueFrom": {"secretKeyRef": {"name": "creds", "key": "key"}}
                    }),
                ]),
                contract_version: "v1".into(),
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "default-image").expect("BYO must produce a plan");
        assert_eq!(plan.kind_str, "BYO");
        assert_eq!(plan.image, "myregistry.azurecr.io/agent:1.0");
        assert_eq!(plan.command.as_deref(), Some(&["python".to_string()][..]));
        assert_eq!(plan.byo_contract_version.as_deref(), Some("v1"));
        assert_eq!(
            plan.runtime_extra_env.get("MODE").map(|s| s.as_str()),
            Some("prod")
        );
        assert_eq!(plan.raw_env.len(), 1);
    }

    // ── plan_openai_agents() — S10.A3 ─────────────────────────────────

    #[test]
    fn plan_openai_agents_uses_default_adapter_image_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Defensive: clear the env var so the test isn't influenced by
        // an operator-side override leaking from the dev shell.
        // SAFETY: serial-test is not on this crate; tests in the same
        // file run on a single thread per Rust's default test harness
        // when --test-threads=1 isn't set, but this env var is read
        // exactly once per call, and we reset it inside the test.
        // Using `unsafe` blocks per Rust 2024.
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let cfg = OpenAIAgentsConfig::default();
        let plan = plan_openai_agents(&cfg);
        assert_eq!(plan.kind_str, "OpenAIAgents");
        assert_eq!(plan.image, DEFAULT_OPENAI_AGENTS_IMAGE);
        assert!(
            plan.command.is_none(),
            "default entrypoint honors image CMD"
        );
        assert!(plan.args.is_none());
        assert!(plan.runtime_extra_env.is_empty());
        assert!(plan.raw_env.is_empty());
        assert!(plan.agent_code.is_none());
        assert!(plan.byo_contract_version.is_none());
    }

    #[test]
    fn plan_openai_agents_passes_through_python_version_and_extra_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let mut extra = BTreeMap::new();
        extra.insert("LOG_LEVEL".into(), "DEBUG".into());
        let cfg = OpenAIAgentsConfig {
            python_version: Some("3.12".into()),
            extra_env: Some(extra),
            ..Default::default()
        };
        let plan = plan_openai_agents(&cfg);
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_PYTHON_VERSION")
                .map(|s| s.as_str()),
            Some("3.12")
        );
        assert_eq!(
            plan.runtime_extra_env.get("LOG_LEVEL").map(|s| s.as_str()),
            Some("DEBUG")
        );
    }

    #[test]
    fn plan_openai_agents_user_extra_env_overrides_python_version_key_when_explicitly_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Defensive: if a user explicitly puts RUNTIME_PYTHON_VERSION in
        // extra_env, their value wins over the producer's
        // python_version-derived default. The merge order
        // (default-first, user-on-top) is the contract.
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let mut extra = BTreeMap::new();
        extra.insert("RUNTIME_PYTHON_VERSION".into(), "3.13".into());
        let cfg = OpenAIAgentsConfig {
            python_version: Some("3.12".into()),
            extra_env: Some(extra),
            ..Default::default()
        };
        let plan = plan_openai_agents(&cfg);
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_PYTHON_VERSION")
                .map(|s| s.as_str()),
            Some("3.13"),
            "user extra_env wins over default (merge order: default-first, user-on-top)"
        );
    }

    #[test]
    fn plan_openai_agents_carries_user_entrypoint_into_command() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let cfg = OpenAIAgentsConfig {
            entrypoint: Some(vec!["python".into(), "-m".into(), "team.researcher".into()]),
            ..Default::default()
        };
        let plan = plan_openai_agents(&cfg);
        assert_eq!(
            plan.command.as_deref(),
            Some(
                &[
                    "python".to_string(),
                    "-m".to_string(),
                    "team.researcher".to_string()
                ][..]
            )
        );
    }

    #[test]
    fn plan_openai_agents_propagates_agent_code() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let agent_code = AgentCodeRef {
            oci: Some(OciAgentCode {
                image: "ghcr.io/team/agent:abc123".into(),
            }),
            ..Default::default()
        };
        let cfg = OpenAIAgentsConfig {
            agent_code: Some(agent_code.clone()),
            ..Default::default()
        };
        let plan = plan_openai_agents(&cfg);
        assert!(
            plan.agent_code.is_some(),
            "agent_code must round-trip from CRD to plan"
        );
    }

    #[test]
    fn openai_agents_default_image_honours_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::set_var(
                "OPENAI_AGENTS_RUNTIME_IMAGE",
                "myacr.azurecr.io/openai-agents:pinned",
            );
        }
        let img = openai_agents_default_image();
        assert_eq!(img, "myacr.azurecr.io/openai-agents:pinned");
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let img = openai_agents_default_image();
        assert_eq!(img, DEFAULT_OPENAI_AGENTS_IMAGE);
    }

    #[test]
    fn openai_agents_default_image_treats_blank_env_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::set_var("OPENAI_AGENTS_RUNTIME_IMAGE", "   ");
        }
        let img = openai_agents_default_image();
        assert_eq!(img, DEFAULT_OPENAI_AGENTS_IMAGE);
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
    }

    #[test]
    fn build_runtime_plan_dispatches_openai_agents_to_producer() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("OPENAI_AGENTS_RUNTIME_IMAGE");
        }
        let rt = RuntimeSpec {
            kind: RuntimeKind::OpenAIAgents,
            openclaw: None,
            openai_agents: Some(OpenAIAgentsConfig {
                python_version: Some("3.12".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored-openclaw-default")
            .expect("OpenAIAgents must produce a plan");
        assert_eq!(plan.kind_str, "OpenAIAgents");
        assert_eq!(plan.image, DEFAULT_OPENAI_AGENTS_IMAGE);
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_PYTHON_VERSION")
                .map(|s| s.as_str()),
            Some("3.12")
        );
    }

    // ── plan_microsoft_agent_framework() — S10.A4 ─────────────────────

    #[test]
    fn plan_maf_uses_default_python_image_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        let cfg = MicrosoftAgentFrameworkConfig::default();
        let plan =
            plan_microsoft_agent_framework(&cfg).expect("default (Python) must produce a plan");
        assert_eq!(plan.kind_str, "MicrosoftAgentFramework");
        assert_eq!(plan.image, DEFAULT_MAF_PYTHON_IMAGE);
        assert!(plan.command.is_none());
        assert!(plan.args.is_none());
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_MAF_LANGUAGE")
                .map(|s| s.as_str()),
            Some("python")
        );
        assert!(plan.raw_env.is_empty());
        assert!(plan.byo_contract_version.is_none());
    }

    #[test]
    fn plan_maf_explicit_python_language_succeeds() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        let cfg = MicrosoftAgentFrameworkConfig {
            language: Some(MafLanguage::Python),
            ..Default::default()
        };
        let plan = plan_microsoft_agent_framework(&cfg).expect("explicit Python must succeed");
        assert_eq!(plan.image, DEFAULT_MAF_PYTHON_IMAGE);
    }

    #[test]
    fn plan_maf_default_language_is_python() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        let cfg = MicrosoftAgentFrameworkConfig::default();
        let plan = plan_microsoft_agent_framework(&cfg).expect("default must succeed");
        assert_eq!(plan.image, DEFAULT_MAF_PYTHON_IMAGE);
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_MAF_LANGUAGE")
                .map(|s| s.as_str()),
            Some("python")
        );
    }

    #[test]
    fn plan_maf_passes_entrypoint_and_extra_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        let mut extra = BTreeMap::new();
        extra.insert("LOG_LEVEL".into(), "DEBUG".into());
        let cfg = MicrosoftAgentFrameworkConfig {
            language: Some(MafLanguage::Python),
            entrypoint: Some(vec!["python".into(), "-m".into(), "team.maf_agent".into()]),
            extra_env: Some(extra),
            ..Default::default()
        };
        let plan = plan_microsoft_agent_framework(&cfg).expect("must succeed");
        assert_eq!(
            plan.command.as_deref(),
            Some(
                &[
                    "python".to_string(),
                    "-m".to_string(),
                    "team.maf_agent".to_string()
                ][..]
            )
        );
        assert_eq!(
            plan.runtime_extra_env.get("LOG_LEVEL").map(|s| s.as_str()),
            Some("DEBUG")
        );
        // RUNTIME_MAF_LANGUAGE controller-default still present alongside user env.
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_MAF_LANGUAGE")
                .map(|s| s.as_str()),
            Some("python")
        );
    }

    #[test]
    fn plan_maf_user_extra_env_overrides_controller_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        let mut extra = BTreeMap::new();
        // Pathological: user pins RUNTIME_MAF_LANGUAGE to a different
        // value. The merge-order contract (default-first, user-on-top)
        // means the user value wins. This is a *feature*, not a bug —
        // it gives the same predictable override semantics across all
        // producers.
        extra.insert("RUNTIME_MAF_LANGUAGE".into(), "experimental".into());
        let cfg = MicrosoftAgentFrameworkConfig {
            language: Some(MafLanguage::Python),
            extra_env: Some(extra),
            ..Default::default()
        };
        let plan = plan_microsoft_agent_framework(&cfg).expect("must succeed");
        assert_eq!(
            plan.runtime_extra_env
                .get("RUNTIME_MAF_LANGUAGE")
                .map(|s| s.as_str()),
            Some("experimental")
        );
    }

    #[test]
    fn maf_python_default_image_honours_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::set_var("MAF_RUNTIME_IMAGE", "myacr.azurecr.io/maf-python:pinned");
        }
        let img = maf_python_default_image();
        assert_eq!(img, "myacr.azurecr.io/maf-python:pinned");
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        assert_eq!(maf_python_default_image(), DEFAULT_MAF_PYTHON_IMAGE);
    }

    #[test]
    fn maf_python_default_image_treats_blank_env_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::set_var("MAF_RUNTIME_IMAGE", "  ");
        }
        let img = maf_python_default_image();
        assert_eq!(img, DEFAULT_MAF_PYTHON_IMAGE);
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
    }

    #[test]
    fn build_runtime_plan_dispatches_maf_python_to_producer() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("MAF_RUNTIME_IMAGE");
        }
        let rt = RuntimeSpec {
            kind: RuntimeKind::MicrosoftAgentFramework,
            openclaw: None,
            microsoft_agent_framework: Some(MicrosoftAgentFrameworkConfig {
                language: Some(MafLanguage::Python),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = build_runtime_plan(&rt, "ignored-openclaw-default")
            .expect("MAF Python must produce a plan");
        assert_eq!(plan.kind_str, "MicrosoftAgentFramework");
        assert_eq!(plan.image, DEFAULT_MAF_PYTHON_IMAGE);
    }
}
