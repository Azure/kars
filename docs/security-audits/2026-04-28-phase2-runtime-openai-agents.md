# Security Audit — Phase 2 / S10.A3 OpenAI Agents Python runtime

**Date:** 2026-04-28
**Slice:** S10.A3 `phase2-runtime-openai-agents`
**Branch:** `phase2-runtime-openai-agents`
**Layer (per `docs/internal/phase-2-story.md` §2):** Layer 1 — Runtime
**§14.6 column moved:** Column 11 (multi-runtime hosting) — *partial credit; column flips fully ✓ on S10.A4 merge per the column-11 ✓ bar.*

---

## 1. Scope

This slice ships the **first non-OpenClaw native runtime** end-to-end through the controller dispatch:

In:
- `plan_openai_agents` producer in `controller::reconciler::runtime` (replaces the `AdapterMissing("OpenAIAgents")` short-circuit landed in S10.A2).
- `DEFAULT_OPENAI_AGENTS_IMAGE` constant + `openai_agents_default_image()` helper reading `OPENAI_AGENTS_RUNTIME_IMAGE` env override (whitespace treated as unset).
- Reconciler `is_byo` flag generalised to `is_openclaw` (positive polarity). `RuntimeKind::OpenAIAgents` now flows through the same generic-runtime container shape as BYO: container name `agent` (not `openclaw`), no OpenClaw-specific env (`OPENCLAW_MODEL`, `OPENCLAW_GATEWAY_TOKEN`, `FOUNDRY_DEPLOYMENTS`, `FOUNDRY_AGENT_ID`, `FOUNDRY_AGENT_TOOLS`), no admin-token mount.
- `sandbox-images/openai-agents/` — Dockerfile (Python 3.12 + `openai-agents` SDK) + `entrypoint.sh` exporting `OPENAI_BASE_URL=http://127.0.0.1:8443/openai/v1` and `AZURECLAW_PLATFORM_MCP_URL=http://127.0.0.1:8443/platform/mcp` (S10.B platform MCP server).
- 8 new tests (7 producer-level + 1 dispatcher-level): default image, env-override image (set + unset + whitespace-as-unset), `python_version`-derived `RUNTIME_PYTHON_VERSION`, `extra_env` merge, user-extra wins on conflict, `entrypoint` → `command` propagation, `agent_code` round-trip, `kind_str == "OpenAIAgents"`, dispatcher arm wiring.
- 1 negative-list test updated: `plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind` drops the `OpenAIAgents` case (4 cases remain: MAF, SemanticKernel, LangGraph, Anthropic).

Out (deferred):
- **In-pod adapter package** (`azureclaw-runtime-openai-agents` PyPI) — AAD token shim for Azure OpenAI, `AZURE_OPENAI_ENDPOINT` rewriting based on `InferencePolicy`, AGT-init compat, OTel SDK wiring. The Dockerfile + entrypoint scaffolding above is `LABEL`-tagged but does not yet consume the adapter; until the package lands, S10.A3 deploys a runnable shell that proxies LLM traffic through the router but has no Python-side governance hooks.
- **Reference example app** (hello-world OpenAI Agent end-to-end on a Kind cluster) — folds into the same follow-up commit as the adapter package.
- **Class B mesh / spawn / handoff tools** — blocked on AgentMesh-Python upstream availability (`docs/internal/agt-upstream-asks.md` §3). S10.A3 ships with **Foundry-shim access only** via S10.B's platform MCP server; mesh tools are deliberately absent rather than reimplemented.
- **e2e Kind test + negative-egress assertion** — folds into S10.A4 (MAF) where ≥2 native runtimes share the e2e harness investment.
- **MAF .NET path** — blocked on AgentMesh.Sdk .NET availability; deferred to Phase 3.

---

## 2. Existing implementation surveyed

Per §0.2 #8 ("no parallel-implementation"):

| Reused seam | File | Why |
|---|---|---|
| `RuntimeDeploymentPlan` struct + `RuntimePlanError` taxonomy | `controller/src/reconciler/runtime.rs` (S10.A2) | Single dispatch shape across runtimes; `plan_openai_agents` returns the same struct as `plan_openclaw` / `plan_byo`. |
| `validate_runtime_shape` defensive guard | `controller/src/reconciler/runtime.rs` (S10.A2) | Already enforces variant/kind matching; no new validator added for OpenAIAgents. |
| Reserved-prefix env-var filter (`AGT_`, `FOUNDRY_AGENT_`, `AZURE_`, `IMDS_`, `AZURECLAW_`) | `controller/src/reconciler/mod.rs:872-904` (S10.A2.b) | Producer hands raw map; deployment builder filters at the rendering boundary. Producer uses `RUNTIME_PYTHON_VERSION` (non-reserved) instead of `AZURECLAW_PYTHON_VERSION` so the user-visible env actually survives the filter. |
| `is_byo` runtime-shape branch (now `is_openclaw`) | `controller/src/reconciler/mod.rs:710-1048` | The OpenClaw-vs-generic split was already in place for BYO. Generalising to `is_openclaw` is a 5-line polarity flip that brings OpenAIAgents into the same generic shape rather than introducing a parallel `is_openai_agents` flag. |
| `RuntimeReady` Condition + `runtimeKind` status field | `controller/src/status/mod.rs` (S10.A1) | Producer-success path stamps `RuntimeReady=True/Reconciled` via the existing running-status patch; no new Condition reason introduced. |
| Platform MCP server (Foundry-shim tools) | `inference-router/src/mcp/platform.rs` (S10.B) | Adapter entrypoint advertises `AZURECLAW_PLATFORM_MCP_URL=http://127.0.0.1:8443/platform/mcp`; no Foundry-tool reimplementation in the adapter. |

No new modules created. No second copy of the dispatch table, image-resolution helper, or container-shape branch.

---

## 3. Threat model

| Threat | S10.A2.b state | What S10.A3 changes |
|---|---|---|
| Operator submits `kind: OpenAIAgents`; controller silently runs the OpenClaw image. | A2.b: explicit `AdapterMissing` skip + 300 s requeue. | A3 wires the adapter image; `image` resolution goes through `openai_agents_default_image()` only. The OpenClaw default image (`ctx.sandbox_image`) is **not consulted** — the producer ignores its argument for non-OpenClaw kinds. Test `build_runtime_plan_dispatches_openai_agents_to_producer` passes `"ignored-openclaw-default"` and asserts the result equals `DEFAULT_OPENAI_AGENTS_IMAGE`. |
| Operator pins `OPENAI_AGENTS_RUNTIME_IMAGE` to a typo (e.g. blank string, whitespace, malicious registry). | n/a (no env override existed) | `openai_agents_default_image()` treats whitespace-only as unset (falls back to default). Test `openai_agents_default_image_treats_blank_env_as_unset` asserts this. Maliciously valued env requires controller-deployment-level access — out of the per-CR threat model. |
| Reserved-prefix env smuggling via `python_version` (e.g. user sets `python_version: '"; export AZURECLAW_FOO=bar'`). | n/a | `python_version` is a `String`; producer copies it verbatim into a *value* (never a key). The deployment builder NUL-byte filter runs on values; reserved-prefix filter runs on keys (`RUNTIME_PYTHON_VERSION` is non-reserved by design). |
| User `extraEnv` collides with the producer's `RUNTIME_PYTHON_VERSION` default and silently overrides controller intent. | n/a | Test `plan_openai_agents_user_extra_env_overrides_python_version_key_when_explicitly_set` asserts the merge order is *intentional* (user wins on conflict). The audit doc records this as a feature, not a bug. |
| OpenAIAgents pod starts as the `openclaw` container name; CLI tooling (`azureclaw connect`, `handoff`, `eval`) hits the OpenClaw container and fails with cryptic errors. | A2.b reserved `agent_container_name = "agent"` for BYO only. | `is_openclaw` polarity flip extends generic-runtime container naming to OpenAIAgents (and any future non-OpenClaw kind). CLI hardcoding of `-c openclaw` (per S10.A1 rubber-duck #4) is unchanged for OpenClaw; OpenAIAgents users will need `-c agent` — documented as a known asymmetry, scheduled for S10.A5 (CLI surface). |
| Direct external LLM egress from inside the adapter pod (bypasses InferencePolicy + Content Safety). | A2.b: egress-guard iptables rule pinned to UID 1000 + NetworkPolicy denies all egress except router + DNS. | A3 inherits both unchanged. The adapter `entrypoint.sh` exports `OPENAI_BASE_URL=http://127.0.0.1:8443/openai/v1` so the SDK's default uses the router; even if user agent code overrides it, the egress-guard blocks the connection. **Negative test deferred to S10.A4** (folds into shared e2e harness). |
| Adapter image declares no contract label, fails the BYO contract check, ships `RuntimeReady=False`. | n/a (BYO-only) | Dockerfile sets `LABEL org.azureclaw.runtime.contract="v1"` so the same contract verifier (Phase 2 warn-only) sees a recognised adapter. Native-runtime contract enforcement is on the same Phase 3 strict-mode track. |

### Residual risks

- **Adapter package not yet published**: S10.A3 deploys a Python container that has the `openai-agents` SDK installed but no AzureClaw-specific Python adapter wired in. A deployer who points `agentCode` at a non-trivial agent today will get LLM-routed traffic (governance ✓) but no AAD shim for Azure OpenAI and no OTel propagation (governance gap). Tracked as **immediate follow-up** before slice closes; Foundry-shim tools via S10.B are unaffected.
- **Class B mesh tools missing**: per the runtime-agnostic rule, mesh / spawn / handoff are per-runtime and ride upstream AgentMesh SDK. AgentMesh-Python doesn't exist on PyPI. OpenAI Agents users running on AzureClaw today can call Foundry tools and use the router governance gate; they cannot mesh-message OpenClaw siblings. Cross-runtime mesh requires upstream AgentMesh-Python publication. Documented in `docs/internal/agt-upstream-asks.md` §3.
- **OpenAIAgents container-name asymmetry**: as noted above, `azureclaw connect/handoff/eval` hardcode `-c openclaw`. S10.A5 ships the CLI dispatch on `runtimeKind`. Until then, OpenAIAgents users need `-c agent`. Operationally surfaceable; not a security risk.

---

## 4. Hard-rule checklist (§0.2)

| Rule | Status |
|---|---|
| #6 No custom crypto | ✓ — no crypto in this slice. |
| #7 No public AAIF / CNCF filing | ✓ — internal slice. |
| #8 No parallel implementation | ✓ — `plan_openai_agents` extends `RuntimeDeploymentPlan`; `is_byo`→`is_openclaw` is a polarity flip, not a duplicate flag. |
| #9 Audit doc with two sign-offs | This document. |
| #10 External-spec citation | OpenAI Agents Python SDK: <https://github.com/openai/openai-agents-python>. |
| No reimplementation of Signal Protocol / X3DH / Double Ratchet / KNOCK / registry / relay | ✓ — Class B explicitly deferred to upstream AgentMesh-Python. See `docs/internal/agt-upstream-asks.md` §3. |

---

## 5. Test coverage delta

| Layer | Before A3 | After A3 | Delta |
|---|---|---|---|
| Controller unit | 307 | 315 | +8 |
| Controller integration | (unchanged) | (unchanged) | 0 |
| Router lib | 608 | 608 | 0 |
| CLI vitest | 435 | 435 | 0 |

New tests:

1. `plan_openai_agents_uses_default_adapter_image_when_env_unset`
2. `plan_openai_agents_passes_through_python_version_and_extra_env`
3. `plan_openai_agents_user_extra_env_overrides_python_version_key_when_explicitly_set`
4. `plan_openai_agents_carries_user_entrypoint_into_command`
5. `plan_openai_agents_propagates_agent_code`
6. `openai_agents_default_image_honours_env_override`
7. `openai_agents_default_image_treats_blank_env_as_unset`
8. `build_runtime_plan_dispatches_openai_agents_to_producer`

Updated test:
- `plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind` — drops `OpenAIAgents` (now wired); 4 cases remain (MAF, SemanticKernel, LangGraph, Anthropic).

---

## 6. AGT / AgentMesh upstream dependencies

Per the runtime-agnostic rule (`plan.md` §S10-runtime-agnostic-rule):

- **AgentMesh-Python** does not exist on PyPI. S10.A3's adapter ships with **Foundry-shim access only** via S10.B; mesh tools are explicitly absent. **AzureClaw will not reimplement Signal Protocol / X3DH / Double Ratchet / KNOCK / registry / relay** in the router or the adapter package. The gap is upstream's to fill.
- Tracking: `docs/internal/agt-upstream-asks.md` §3 (gitignored). Talking points for the next AGT/AgentMesh sync are recorded there.

---

## 7. Sign-offs

- [ ] Plan owner: pallakatos
- [ ] Reviewer: (to fill)
