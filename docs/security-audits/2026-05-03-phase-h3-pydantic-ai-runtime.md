# Phase H#3 — Pydantic-AI runtime adapter (security audit)

**Date:** 2026-05-03
**PR:** #181 (target branch: `dev`)
**Scope:** First-class runtime support for the [Pydantic-AI](https://ai.pydantic.dev/)
agent framework. Adds `RuntimeKind::PydanticAi` to the controller, a
producer in `reconciler/runtime.rs`, the `runtimes/pydantic-ai`
in-pod adapter package, and the `sandbox-images/pydantic-ai`
container image. Wires the new variant into the CRD and the E2E gate.

## Why a dedicated adapter (vs. BYO)

Pydantic-AI is provider-agnostic — a single `Agent` definition can
target OpenAI, Azure OpenAI, Anthropic, Gemini, etc. Each provider
SDK reads its base URL + API key from process env at construction
time. BYO would require every Pydantic-AI user to re-implement the
same env-pinning + sentinel logic the LangGraph adapter already
solved. Shipping it as a first-class runtime lets users supply only
their agent code.

## Threat model

| Threat | Mitigation |
|---|---|
| Direct egress to public LLM endpoints | Adapter pins `OPENAI_BASE_URL`, `AZURE_OPENAI_ENDPOINT`, `ANTHROPIC_BASE_URL` to `127.0.0.1:8443`; egress-guard iptables init container drops UID-1000 packets to non-loopback / non-DNS targets. |
| Real provider API keys land in pod env | Each `*_API_KEY` env is set to the sentinel `router-managed`. The router substitutes the AAD-attested credential on egress. No real key ever reaches the pod. |
| Adapter bypassing AGT governance | All LLM traffic flows through the inference-router sidecar which gates on AGT trust score, runs Foundry Content Safety, emits OTel GenAI spans, and applies token budgets. |
| Container running as root | `Dockerfile USER 1000`. Workspace `/sandbox/agent` chowned to UID 1000. Reconciler-enforced runAsUser/runAsNonRoot pin in `reconciler/mod.rs`. |
| Producer leaking reserved env keys | `plan_pydantic_ai` only emits `RUNTIME_PYTHON_VERSION` plus the user's `extra_env`; the deployment builder strips `AGT_*` / `AZURE_*` / `AZURECLAW_*` reserved prefixes from any user-supplied env. Unit tests assert the producer emits no `AZURECLAW_*` keys. |
| Wrong variant struct accepted at admission | CEL rule `(self.kind == 'PydanticAi') == has(self.pydanticAi)` plus the in-process `validate_runtime_shape` mirror catch all combinations. |

## What was added

### Controller / CRD

* `RuntimeKind::PydanticAi` enum variant (PascalCase wire format).
* `RuntimeSpec.pydantic_ai: Option<PydanticAiConfig>` field.
* `PydanticAiConfig` struct: `python_version`, `agent_code`,
  `entrypoint`, `extra_env` — same shape as `AnthropicConfig` /
  `LangGraphConfig`.
* `DEFAULT_PYDANTIC_AI_IMAGE` const + `pydantic_ai_default_image()`
  helper honouring the `PYDANTIC_AI_RUNTIME_IMAGE` env override.
* `plan_pydantic_ai` producer threading `python_version` into
  `RUNTIME_PYTHON_VERSION` and merging user `extra_env` last.
* CRD `kind` enum + `runtimeKind` printer-column enum extended.
* CRD CEL rule + `pydanticAi` property block (mirrors `anthropic`).

### Runtime adapter

`runtimes/pydantic-ai/`:

* `pyproject.toml` declares `pydantic-ai>=0.0.13,<1` plus the same
  `azure-identity`, `opentelemetry-*`, `httpx`, `a2a_agentmesh`,
  `agent_sandbox` deps as the LangGraph adapter.
* `runtime.py::bootstrap()` is idempotent
  (`__AZURECLAW_RUNTIME_INITIALIZED__=1` guard), pins three provider
  base URLs to the router proxy, sets sentinel API keys, initializes
  OTel, installs SIGTERM/SIGINT handlers.
* `aad.py`, `mesh.py`, `otel.py` ported verbatim from the LangGraph
  adapter (same router proxy contract).

### Sandbox image

`sandbox-images/pydantic-ai/Dockerfile`:

* `FROM mcr.microsoft.com/cbl-mariner/base/python:3.12 AS base`.
* Installs AGT-Python wheels then the `azureclaw_runtime_pydantic_ai`
  package.
* Labels `org.azureclaw.runtime.kind="PydanticAi"`,
  `org.azureclaw.runtime.contract="v1"`,
  `org.azureclaw.runtime.language="python"`.
* `USER 1000`, workdir `/sandbox/agent`, entrypoint
  `azureclaw-pydantic-ai-entrypoint.sh`.

`entrypoint.sh` pins all three provider URLs + sentinel API keys,
exports `AZURECLAW_PLATFORM_MCP_URL`, `AZURECLAW_AGT_RELAY_URL`,
`AZURECLAW_AGT_REGISTRY_URL`, `OTEL_EXPORTER_OTLP_ENDPOINT`, then
calls `bootstrap()` and `exec`s the user agent.

### Tests

Controller (44 tests in `reconciler::runtime`, +4 new):

* `pydantic_ai_default_image_falls_back_when_env_unset`
* `plan_pydantic_ai_emits_pydanticai_kind_str_and_default_image`
* `plan_pydantic_ai_threads_python_version_and_user_extra_env`
* `plan_pydantic_ai_user_extra_env_overrides_producer_default`

Plus the existing parametrized validate tests are extended to cover
the new variant (`validate_rejects_each_tier2_kind_without_struct`,
`validate_accepts_each_tier2_kind_with_correct_struct`,
`kind_str_returns_pinned_wire_strings`).

E2E (`tests/e2e/run.sh`):

* `test_runtime_pydantic_ai` — applies a `ClawSandbox` of kind
  `PydanticAi` and asserts the namespace materializes and (when a
  Deployment is produced) the agent container image references the
  pydantic-ai runtime tag.
* Registered in the `case "$RUNTIME"` dispatcher under
  `pydantic-ai` and in the `all` lane.

## AGT boundary respect

The adapter does **not** reimplement trust scoring, policy
enforcement, or audit. All of those happen in the inference-router
which calls into the upstream `agentmesh` crate (Microsoft AGT).
The adapter is purely a bootstrapper: it pins env vars and exits.

## Verification

* `cargo build --package azureclaw-controller` — clean
* `cargo clippy --package azureclaw-controller --all-targets -- -D warnings` — clean
* `cargo test --package azureclaw-controller -- --test-threads=1` — 484 passed
* `cargo fmt --all` — no diff

## Out of scope (follow-up)

* Building and pushing the `azureclaw-runtime-pydantic-ai` image to
  the production ACR — that's an ops PR, not a code PR.
* Pydantic-AI Logfire integration — Pydantic-AI emits OTel natively,
  so the existing OTel pipeline already captures it. No special
  Logfire bridge needed for now.
