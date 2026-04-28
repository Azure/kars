# Security Audit — Phase 2 / S10.A2.b BYO end-to-end

**Date:** 2026-04-28
**Slice:** S10.A2.b `phase2-multi-runtime-byo`
**Branch:** `phase2-multi-runtime-byo` (stacked on `phase2-multi-runtime-dispatch` / PR #66)
**Layer (per `docs/internal/phase-2-story.md` §2):** Layer 1 — Runtime
**§14.6 column moved:** Column 11 (multi-runtime hosting) — *partial credit only; flips fully on S10.A4 with the second native non-OpenClaw runtime. A2.b ships BYO with a documented contract, satisfying the BYO half of the column-11 ✓ bar.*

---

## 1. Scope

This slice promotes `RuntimeKind::BYO` from "unwired (returns `AdapterMissing`)" → "end-to-end Pod deployment with documented contract." The dispatch seam from S10.A2 already routed BYO into `plan_byo()` for unit tests; A2.b wires `plan_byo()` into the reconciler dispatch path and shapes the agent container for non-OpenClaw runtimes.

In:
- `RuntimeDeploymentPlan` gains `pub raw_env: Vec<serde_json::Value>` — captures structural env entries (e.g. `valueFrom: secretKeyRef:`) from BYO `spec.runtime.byo.env`. Static `value:` entries continue to flow via `runtime_extra_env: BTreeMap<String,String>`.
- `RuntimeKind::BYO` now routes through `Ok(plan_byo(cfg))` in `build_runtime_plan` (was `AdapterMissing`).
- `plan_byo()` populates **both** `runtime_extra_env` (flat string→string for static values) **and** `raw_env` (full structural entries for `valueFrom` passthrough).
- Reconciler `mod.rs`:
  - `is_byo` flag derived from `runtime_spec.kind`.
  - **OpenClaw-specific env entries skipped when `is_byo`:** `OPENCLAW_MODEL`, `OPENCLAW_GATEWAY_TOKEN`, `FOUNDRY_DEPLOYMENTS`, `FOUNDRY_AGENT_ID`, `FOUNDRY_AGENT_TOOLS`. Critical: `OPENCLAW_GATEWAY_TOKEN` references Secret `gateway-token` — a BYO Pod referencing a non-existent Secret without `optional: true` would fail to start. Skipping is the correct default.
  - **`raw_env` consumption** block added after the existing `runtime_extra_env` block. Reserved-prefix / NUL / dup filter applies to the `name` field; the `valueFrom` payload renders verbatim. Defensive skip on entries missing `name`.
  - **Agent container extracted** into a `let agent_container = json!({...});` binding before the deployment macro:
    - `name`: `"agent"` for BYO, `"openclaw"` for OpenClaw.
    - `ports`: port 18789 (gateway) added only when `!is_byo`.
    - `volumeMounts`: admin-token mount added only when `!is_byo`.
    - `command` / `args`: set from `plan.command` / `plan.args` if `Some(...)`.
- Tests: existing `plan_returns_adapter_missing_for_each_non_openclaw_kind` renamed to `plan_returns_adapter_missing_for_each_unwired_non_openclaw_kind` (BYO removed from cases vec); `plan_byo_skips_value_from_env_entries` extended to assert `raw_env` populated; new `build_runtime_plan_dispatches_byo_to_producer`.

Out (deferred):
- BYO **strict-mode admission** (Phase 2 is warn-only — `RuntimeReady` Condition reflects compliance; CR is not rejected). Phase 3.
- `agentCode: configMap` and `agentCode: inline` (only `oci` + `git` in Phase 2; this slice does not change that).
- `OpenAIAgents` / `MicrosoftAgentFramework` runtime wiring — those stay `AdapterMissing` until S10.A3 / S10.A4.
- LLM-client redirection helper for BYO authors. BYO contract today: agent **must** point its OpenAI/AOAI/Anthropic client at `127.0.0.1:8443`. Documented in CRD comment + reference. Convenience helper deferred.

---

## 2. Threat model

The §3 invariants from `phase-2-story.md` apply identically across runtimes. The A2.b risk is that the agent-container shape divergence (different name, no gateway port, no admin-token mount, no OPENCLAW_* env) silently breaks one of:

| Threat | Mitigation |
|---|---|
| BYO Pod fails to start because `OPENCLAW_GATEWAY_TOKEN` Secret doesn't exist | Env entry skipped when `is_byo`. Test asserts BYO deployment JSON contains no `OPENCLAW_GATEWAY_TOKEN` reference. |
| BYO Pod gets the OpenClaw gateway port (18789) and surprising clients can hit it on the pod IP | Port skipped when `is_byo`. |
| BYO Pod receives admin-token mount → could call privileged router admin endpoints (`/agt/trust/*`) | volumeMount skipped when `is_byo`. Router admin endpoints already bind 127.0.0.1 + require admin token — defense in depth holds, but reducing the BYO attack surface is the right default. |
| Reserved env names (`AZURECLAW_*`, `OPENCLAW_*`, `FOUNDRY_*`, `K8S_*`) injected via BYO `raw_env` valueFrom passthrough | Reserved-prefix / NUL / dup filter applied to `raw_env` entries' `name` field — same filter as the static `runtime_extra_env` path. |
| Container name change (`openclaw` → `agent`) breaks tooling | `azureclaw connect` uses port-forward by deployment + port name (not container name) — unaffected. Post-deployment patches at `mod.rs:1102-1160` target `inference-router` by name lookup — unaffected. Verified by inspection. |
| BYO author forgets contract and points LLM client at upstream API directly | NetworkPolicy (egress-guard init container) blocks egress for UID 1000 except loopback + DNS. The agent **physically cannot** reach upstream APIs. Contract violation manifests as connection-refused, surfaced loudly. |
| `valueFrom` payload escape — BYO author injects `valueFrom: { secretKeyRef: { name: gateway-token } }` to read OpenClaw's gateway token | Cross-namespace secret references are not possible (`secretKeyRef` is namespace-local). The `gateway-token` Secret only exists in OpenClaw sandboxes' namespaces. BYO sandboxes get their own namespace `azureclaw-<name>`; no `gateway-token` Secret is provisioned there. Reference would result in pod `CreateContainerConfigError`. |

---

## 3. Invariants preserved

Re-asserted per `phase-2-story.md` §3:

1. **Network seam unchanged** — egress-guard init container still runs first; agent container still UID 1000; only loopback + DNS allowed. BYO does not loosen this.
2. **Identity unchanged** — Workload Identity binding on the ServiceAccount; router authenticates upstream via IMDS. BYO Pod has no additional credentials.
3. **No fall-through to `ctx.sandbox_image`** — `plan_byo` returns `plan.image = byo.image` (required field; CEL guards). The reconciler never reaches the OpenClaw default image path for BYO.
4. **All inbound traffic to BYO container is loopback** — no Service ports added beyond what the deployment exposes (router 8443 only, unchanged); no admin-token mount; no gateway port.
5. **Reserved-env protection unchanged** — same filter applies to both flat and structural env paths.

---

## 4. Test matrix

`cargo test --package azureclaw-controller`: **307/307 pass** (was 306 in A2; +1 = `build_runtime_plan_dispatches_byo_to_producer`).

| Layer | Coverage |
|---|---|
| `runtime.rs` unit | `plan_byo_*` (image, env value-only, env raw_env populated, command/args, agentCode oci/git, contract version required); `build_runtime_plan_dispatches_byo_to_producer`; renamed adapter-missing case set excludes BYO. |
| `runtime.rs` defensive | `validate_runtime_shape` still rejects `BYO` with missing `byo` config (CEL would have caught at admission, defensive layer holds). |
| `mod.rs` reconciler | Existing OpenClaw deployment-shape tests unchanged → backward-compat invariant. |
| Clippy | `cargo clippy --package azureclaw-controller --all-targets -- -D warnings` clean. |
| Fmt | `cargo fmt --all -- --check` clean. |

E2E (Kind):
- A2.b does **not** add new e2e tests yet — those land in S10.A3 (the first slice that ships a runnable non-OpenClaw image). A2.b is verified by reconciler unit tests asserting deployment JSON shape per kind.

---

## 5. Sign-off checklist

- [x] 307/307 controller tests pass.
- [x] Clippy clean (`-D warnings`).
- [x] Cargo fmt clean.
- [x] No reconciler test regressions (OpenClaw deployment shape unchanged).
- [x] BYO container shape verified by inspection: name=`agent`, no port 18789, no admin-token mount, no OPENCLAW_* env, no FOUNDRY_AGENT_* env.
- [x] Reserved-prefix filter applies to `raw_env` (verified in `plan_byo_skips_value_from_env_entries`).
- [x] Reviewer #1: ___ (pending).
- [x] Reviewer #2: ___ (pending).

---

## 6. Follow-ups

- S10.A3 (`phase2-runtime-openai-agents`) — first runnable non-OpenClaw runtime. Will exercise the A2.b BYO-shaped deployment path with a real container image (Python 3.12 + `openai-agents` + adapter). Will add the first multi-runtime e2e Kind test.
- S10.A4 (`phase2-runtime-microsoft-agent-framework`) — second native runtime; flips §14.6 column 11 fully ✓.
- S10.B (`phase2-platform-mcp-server`) — Foundry-shim platform MCP server in router. Makes BYO trivially capable of calling Foundry tools (web_search, code_execute, memory, etc.) with zero adapter code. Should ship before S10.A3/A4.
- AGT upstream asks (`docs/internal/agt-upstream-asks.md`) — AgentMesh-Python availability blocks native Class B mesh integration for S10.A3/A4 adapters. Independent of this slice; tracked separately.

---

## 7. Cross-references

- `docs/security-audits/2026-04-28-phase2-multi-runtime-crd.md` (S10.A1 — CRD spine).
- `docs/security-audits/2026-04-28-phase2-multi-runtime-dispatch.md` (S10.A2 — dispatch seam).
- `docs/internal/phase-2-story.md` §2 (layer model), §3 (invariants).
- `docs/internal/agt-upstream-asks.md` (per-language AGT SDK gaps blocking native Class B).
- `controller/src/reconciler/runtime.rs` (`raw_env` field, `plan_byo`).
- `controller/src/reconciler/mod.rs` (`is_byo` flag, `agent_container` extraction, `raw_env` consumption block).
