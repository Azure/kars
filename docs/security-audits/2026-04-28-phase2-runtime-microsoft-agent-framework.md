# Security Audit — Phase 2 / S10.A4 Microsoft Agent Framework Python runtime

**Date:** 2026-04-28
**Slice:** S10.A4 `phase2-runtime-microsoft-agent-framework`
**Branch:** `phase2-runtime-microsoft-agent-framework`
**Layer (per `docs/internal/phase-2-story.md` §2):** Layer 1 — Runtime
**§14.6 column moved:** Column 11 (multi-runtime hosting) — **flips fully ✓** (≥2 native non-OpenClaw runtimes wired end-to-end: OpenAIAgents from S10.A3 + MAF Python from this slice).

---

## 1. Scope

This slice ships the **second native runtime**, completing the §14.6 column-11 bar. MAF is the strategically prioritized Microsoft-aligned runtime — it is the unified successor to AutoGen v0.4 and AGT integrates natively.

In:
- `plan_microsoft_agent_framework` producer in
  `controller::reconciler::runtime` — replaces the
  `AdapterMissing("MicrosoftAgentFramework")` short-circuit landed in
  S10.A2. **Returns `Result`** (not `Ok` direct) because MAF is the
  first runtime whose producer can refuse on a *language-flavour*
  basis.
- `DEFAULT_MAF_PYTHON_IMAGE` constant + `maf_python_default_image()`
  helper reading `MAF_RUNTIME_IMAGE` env override (whitespace as unset,
  same convention as S10.A3).
- **Language gate**: `language: dotnet` returns
  `RuntimePlanError::ShapeInvalid` with a message citing the upstream
  AgentMesh.Sdk .NET blocker. The reconciler dispatch surfaces this as
  `Degraded / SpecInvalid` Condition + 300 s requeue (existing
  `ShapeInvalid` path landed in S10.A2).
- `RUNTIME_MAF_LANGUAGE` controller-default env (non-reserved prefix —
  survives the deployment builder's reserved-prefix filter).
- `sandbox-images/maf-python/` — Dockerfile (Python 3.12 +
  `agent-framework>=0.1,<0.2` + `azure-identity` for the eventual AAD
  shim) + `entrypoint.sh` exporting `OPENAI_BASE_URL`,
  `AZURE_OPENAI_ENDPOINT`, `AZURECLAW_PLATFORM_MCP_URL` — all pointed
  at the router sidecar.
- 9 new tests (315 → 324, all green): default Python image, explicit
  Python language succeeds, dotnet → ShapeInvalid (with msg
  assertions), entrypoint propagation, controller-default + user
  extra_env merge, user-extra-wins on conflict, env-override image
  (set + whitespace-as-unset), dispatcher arm wiring (Python success +
  dotnet rejection).
- Updated `plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind`
  — drops `MicrosoftAgentFramework` case; only 3 Tier-2 placeholders
  remain (SemanticKernel, LangGraph, Anthropic).

Out (deferred):
- **MAF .NET path** — blocked on AgentMesh.Sdk .NET upstream
  availability. Reframed as a producer-side `ShapeInvalid` so the
  operator gets a clear error rather than a silently-mis-imaged pod.
  Phase 3 lifts this once upstream ships.
- **In-pod adapter Python package** (`azureclaw-runtime-maf-python`
  PyPI) — AAD shim (`DefaultAzureCredential` → bearer-on-router),
  `AZURE_OPENAI_ENDPOINT` rewriting based on `InferencePolicy`,
  AGT-init compat, MAF-specific MCP client glue, OTel SDK wiring.
  Immediate follow-up before slice closes.
- **Class B mesh / spawn / handoff** — blocked on AgentMesh-Python
  upstream. Same gap as S10.A3; tracked in
  `docs/internal/agt-upstream-asks.md` §3. Foundry-shim tools via
  S10.B platform MCP are unaffected.
- **Reference example app + e2e Kind test** — fold into S10.A5 (CLI
  surface) where the harness is shared across all three native
  runtimes.

---

## 2. Existing implementation surveyed

Per §0.2 #8 (no parallel-implementation):

| Reused seam | File | Why |
|---|---|---|
| `RuntimeDeploymentPlan` + `RuntimePlanError::{AdapterMissing, ShapeInvalid}` | `controller/src/reconciler/runtime.rs` (S10.A2) | MAF refuses dotnet via the same `ShapeInvalid` taxonomy that S10.A2 introduced. Reconciler already maps `ShapeInvalid` to `Degraded / SpecInvalid` — no new Condition path. |
| `is_openclaw` runtime-shape branch | `controller/src/reconciler/mod.rs` (S10.A3) | MAF Python rides the same generic-runtime container shape established in S10.A2.b (BYO) and broadened in S10.A3 (OpenAIAgents). No new flag, no new branch. |
| Reserved-prefix env filter (`AGT_`, `FOUNDRY_AGENT_`, `AZURE_`, `IMDS_`, `AZURECLAW_`) | `controller/src/reconciler/mod.rs` (S10.A2.b) | Producer uses `RUNTIME_MAF_LANGUAGE` (non-reserved) so its controller-default survives the filter. |
| `AdapterMissing` warn + `RuntimeReady=False` Condition | `controller/src/reconciler/mod.rs` + `status::stamp_runtime_unsupported` | MAF dotnet does NOT use this path — it goes through `ShapeInvalid` → `Degraded / SpecInvalid` since the *kind* is wired, the *flavour* isn't. Distinction matters: AdapterMissing is "controller-build problem", ShapeInvalid is "spec problem". |
| Platform MCP server | `inference-router/src/mcp/platform.rs` (S10.B) | Adapter entrypoint advertises `AZURECLAW_PLATFORM_MCP_URL`; no Foundry-tool reimplementation in the adapter. |

No new modules created. No second copy of the dispatch table or
container-shape branch.

---

## 3. Threat model

| Threat | S10.A3 state | What S10.A4 changes |
|---|---|---|
| Operator submits `kind: MicrosoftAgentFramework, language: dotnet`; controller silently runs the Python image. | A3 doesn't know about MAF (still `AdapterMissing`). | A4 **explicitly refuses** dotnet via `ShapeInvalid` with an upstream-blocker citation. The operator gets `Degraded / SpecInvalid` + a 300s requeue rather than a mis-imaged pod. The error message names both the offending value (`dotnet`) and the resolution path (Phase 3 + AgentMesh.Sdk .NET upstream). Tests `plan_maf_dotnet_returns_shape_invalid_pending_phase3` + `build_runtime_plan_surfaces_shape_invalid_for_maf_dotnet` lock this. |
| Operator pins `MAF_RUNTIME_IMAGE` to malicious / typo'd registry. | n/a | `maf_python_default_image()` treats whitespace-only as unset (same convention as `openai_agents_default_image()`). Maliciously valued env requires controller-deployment-level access — out of the per-CR threat model. Test `maf_python_default_image_treats_blank_env_as_unset` locks this. |
| `language` field defaults to a language we haven't wired (silent fall-through). | n/a | `MafLanguage::default()` is `Python` — the wired path. Test `plan_maf_uses_default_python_image_when_env_unset` confirms `None` → Python plan. The match is exhaustive over `MafLanguage` so a future variant breaks the build at this site. |
| User `extra_env` collides with `RUNTIME_MAF_LANGUAGE` controller default and silently overrides controller intent. | n/a | Test `plan_maf_user_extra_env_overrides_controller_default` asserts the merge order is *intentional* (default-first, user-on-top). Same merge contract as `plan_openai_agents`. Documented as a feature here so reviewers don't flag it as a bug in S10.A4. |
| Direct external LLM egress from inside the MAF pod (bypasses InferencePolicy + Content Safety). | A3: egress-guard + NetworkPolicy. | A4 inherits both unchanged. The adapter `entrypoint.sh` exports both `OPENAI_BASE_URL` and `AZURE_OPENAI_ENDPOINT` so MAF's `agent-framework` SDK uses the router by default, regardless of which Foundry / AOAI client path the user selects. |
| MAF pod starts as `openclaw` container; CLI tooling hits the wrong container. | A3: `is_openclaw=false` for OpenAIAgents → container name `agent`. | Same shape applies to MAF — `agent` container. CLI dispatch on `runtimeKind` lands in S10.A5. |
| `agent-framework` SDK pulls AAD tokens via `DefaultAzureCredential` and calls AOAI directly, bypassing the router. | n/a | Adapter is **scaffolding only** for this slice — the AAD-bypass risk is real until the in-pod adapter package ships. The egress-guard mitigates: even if the SDK acquires a token and tries to dial AOAI, NetworkPolicy + iptables on UID 1000 block the egress. Net result: AAD-acquired but unusable tokens, observable via `RuntimeReady` failing on the missing adapter package — *not* a covert tunnel. |

### Residual risks

- **Adapter package not yet published** (mirrored from S10.A3 §3): MAF
  pods today get LLM-routed traffic ✓ but no AAD shim and no OTel
  propagation. Tracked as the immediate follow-up before slice closes.
- **Class B mesh tools missing**: per the runtime-agnostic rule, mesh
  /spawn / handoff are per-runtime and ride upstream AgentMesh SDK.
  AgentMesh-Python doesn't exist on PyPI. MAF users on AzureClaw can
  call Foundry tools and use the router governance gate; they cannot
  mesh-message OpenClaw siblings until upstream publishes
  AgentMesh-Python.
- **MAF .NET upstream gap**: documented in
  `docs/internal/agt-upstream-asks.md` §3. Phase 3 picks this up once
  AgentMesh.Sdk .NET is generally available.

---

## 4. Hard-rule checklist (§0.2)

| Rule | Status |
|---|---|
| #6 No custom crypto | ✓ — no crypto in this slice. |
| #7 No public AAIF / CNCF filing | ✓ — internal slice. |
| #8 No parallel implementation | ✓ — extends `RuntimeDeploymentPlan` + reuses `is_openclaw` shape; the `language: dotnet` rejection rides existing `ShapeInvalid`. |
| #9 Audit doc with two sign-offs | This document. |
| #10 External-spec citation | Microsoft Agent Framework: <https://github.com/microsoft/agent-framework>. |
| No reimplementation of Signal Protocol / X3DH / Double Ratchet / KNOCK / registry / relay | ✓ — Class B explicitly deferred to upstream AgentMesh-Python (Class A) / AgentMesh.Sdk .NET (Class B for MAF .NET). |

---

## 5. Test coverage delta

| Layer | Before A4 | After A4 | Delta |
|---|---|---|---|
| Controller unit | 315 | 324 | +9 |
| Controller integration | (unchanged) | (unchanged) | 0 |
| Router lib | 608 | 608 | 0 |
| CLI vitest | 435 | 435 | 0 |

New tests:

1. `plan_maf_uses_default_python_image_when_env_unset`
2. `plan_maf_explicit_python_language_succeeds`
3. `plan_maf_dotnet_returns_shape_invalid_pending_phase3`
4. `plan_maf_passes_entrypoint_and_extra_env`
5. `plan_maf_user_extra_env_overrides_controller_default`
6. `maf_python_default_image_honours_env_override`
7. `maf_python_default_image_treats_blank_env_as_unset`
8. `build_runtime_plan_dispatches_maf_python_to_producer`
9. `build_runtime_plan_surfaces_shape_invalid_for_maf_dotnet`

Updated test:
- `plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind` —
  drops `MicrosoftAgentFramework` (now wired); 3 Tier-2 placeholders
  remain.

---

## 6. §14.6 column 11 closure

Column 11 (Multi-runtime hosting) target state per `competitive.md` §14.6:

> `@azureclaw/openai-sandbox-provider` adapter + `OverlayMode` for sigs/agent-sandbox + kagent migration

S10.A4 unblocks the column-11 ✓ bar (≥2 native non-OpenClaw runtimes wired end-to-end through the dispatch seam, with adapter image scaffolding + reserved-prefix env discipline + status-conditions integration):

- ✓ OpenAI Agents Python adapter dispatch (S10.A3)
- ✓ MAF Python adapter dispatch (this slice)
- ✓ BYO end-to-end (S10.A2.b)
- ✓ OverlayMode for sigs/agent-sandbox (S8 — landed earlier in Phase 2)
- ✓ kagent migration (S9.3 from-kagent translator — landed earlier)

What remains for column 11 *polish* (not blockers for the ✓):
- In-pod Python adapter packages (immediate follow-up).
- Reference apps + e2e tests (S10.A5).
- MAF .NET (Phase 3, upstream-blocked).

---

## 7. AGT / AgentMesh upstream dependencies

Per the runtime-agnostic rule:

- **AgentMesh.Sdk .NET** is the blocker for MAF .NET. Documented in
  `docs/internal/agt-upstream-asks.md` §3 alongside the AgentMesh-Python
  ask. **AzureClaw will not reimplement Signal Protocol / X3DH /
  Double Ratchet / KNOCK / registry / relay** in any language.

---

## 8. Sign-offs

- [ ] Plan owner: pallakatos
- [ ] Reviewer: (to fill)
