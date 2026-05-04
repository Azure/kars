# Phase 2 — Multi-runtime CLI surface (S10.A5)

**Date:** 2026-04-28
**Slice:** S10.A5 `phase2-runtime-cli`
**Branch:** `phase2-runtime-cli`
**Plan reference:** `docs/implementation-plan.md` §8; session-plan §S10.A5.
**Predecessors:** S10.A1 (CRD reshape, PR #65), S10.A2 (controller dispatch, PR #66/67), S10.B (platform MCP, PR #68), S10.A3 (OpenAI Agents Python, PR #69), S10.A4 (Microsoft Agent Framework Python, PR #70).

## Scope

Extend the CLI to drive the four wired runtimes end-to-end:

- `azureclaw add --runtime <openclaw|openai-agents|microsoft-agent-framework|byo>` emits the correct `spec.runtime.{kind,variant}` block for each kind.
- `azureclaw add --runtime byo --byo-image <ref> [--byo-contract-version v1]` requires explicit image, defaults `contractVersion: "v1"`.
- `azureclaw add --runtime microsoft-agent-framework [--maf-language python]` defaults to `python`; rejects `dotnet` client-side with a clear "Phase 3 / AgentMesh.Sdk .NET upstream-blocked" message (mirrors the controller's `RuntimePlanError::ShapeInvalid` from S10.A4 but fails before the apply round-trip).
- `azureclaw connect <name>` reads `spec.runtime.kind` from the live CR and addresses the correct container with `kubectl exec -c <name>` — `openclaw` for OpenClaw, `agent` for everything else (mirrors `is_openclaw` polarity at `controller/src/reconciler/mod.rs:710` / `:990`).
- `azureclaw list` adds a `RUNTIME` column.
- Tier-2 placeholders (`SemanticKernel`, `LangGraph`, `Anthropic`) are rejected at the CLI boundary with a "no adapter wired" error so operators get immediate feedback rather than a `RuntimeReady=False / AdapterMissing` Conditions stamp after apply.

A new module `cli/src/runtime.ts` is the single source of truth for the kebab-case ↔ PascalCase mapping, the wired-vs-Tier-2 set, the container-name polarity, and the `spec.runtime` block shape. It is consumed by `add.ts`, `connect.ts`, and `list.ts`. Future runtime additions (Phase 3 SK / LG / Anthropic) plug into this one file.

## Out of scope

- `azureclaw push` runtime parity (handled at controller level via per-runtime image overrides in `runtime.rs`; CLI doesn't override).
- `handoff.ts` / `eval.ts` / `operator.ts` / `up.ts` container-name dispatch — they hardcode `-c openclaw` for legacy reasons; tracked as a follow-up cleanup but not blocking column 11. These commands run only against existing OpenClaw sandboxes today.
- Sub-agent / plugin scaffolding for non-OpenClaw runtimes (Class B per §S10-runtime-agnostic-rule — upstream AGT SDK responsibility).

## Hard-rule checklist (§0.2)

- [x] **#1 No duplication** — extends existing `add.ts` / `connect.ts` / `list.ts` rather than parallel-implementing; the controller's `RuntimeKind` enum stays the single source of truth, mirrored as a closed type.
- [x] **#3 No dead schema** — only the four wired kinds (`OpenClaw`, `OpenAIAgents`, `MicrosoftAgentFramework`, `BYO`) accept a CLI flag; Tier-2 kinds throw with a discoverable error listing the wired set.
- [x] **#8 No custom crypto** — no crypto in this slice.
- [x] **#9 Audit doc** — this file.
- [x] **#10 Verify, don't guess** — `agentContainerName` polarity verified against `controller/src/reconciler/mod.rs:990`; `flagToKind` mapping verified against `crd.rs::RuntimeKind` serde renames.

## Test coverage

- `cli/src/runtime.test.ts` (NEW, 19 tests): `flagToKind` happy/unknown/case-insensitive; `assertRuntimeWired` accepts wired, rejects Tier-2 with discoverable message; `agentContainerName` OpenClaw vs everything-else polarity (incl. all three Tier-2 kinds); `runtimeKindFromCr` reads canonical field, falls back to `OpenClaw` for legacy/missing/null/unknown values; `buildRuntimeBlock` happy paths for all four wired kinds + BYO requires image + MAF rejects dotnet + custom contract version respected + `--image` override only on OpenClaw.
- `cli/src/commands/add.test.ts` — existing 27 tests still pass (manifest-building logic unchanged for OpenClaw default path).
- Full vitest run: **454 passed | 2 skipped** (was 435 / 2 before this slice; +19 from `runtime.test.ts`).
- `npx tsc --noEmit`: clean.
- `npm run lint`: 26 pre-existing warnings in `plugin.ts` (no new lint diagnostics).
- `npm run build`: clean.

## Threat model

| Concern | Mitigation |
|---|---|
| Operator picks a Tier-2 runtime, gets a confusing `RuntimeReady=False` after apply | CLI fails fast with `assertRuntimeWired` listing the four wired kinds + a "Phase 3 roadmap" hint. |
| Operator sets `--runtime byo` without `--byo-image` and gets a CEL admission error from the API server | CLI throws client-side: "`--byo-image` is required when `--runtime byo`. Image must declare `org.azureclaw.runtime.contract=v1`". |
| Operator sets `--maf-language dotnet` | Rejected client-side with explicit "Phase 3 / AgentMesh.Sdk .NET upstream-blocked" message; no malformed CR ever reaches the API server. |
| `connect` against a legacy OpenClaw CR (no `spec.runtime.kind`) | `runtimeKindFromCr` falls back to `OpenClaw` → container name `openclaw` → backward-compatible. |
| `connect` against a CR with an unknown `kind` (e.g. controller older than CLI) | Falls back to `OpenClaw` (defensive); user gets "container not found" only if the live container differs, with kubectl's standard error. |
| Reserved-prefix env (`AZURECLAW_*`, `AGT_*`, `FOUNDRY_AGENT_*`, `AZURE_*`, `IMDS_*`) snuck in via flags | Not exposed as a flag; reserved-prefix filter is enforced in the controller's deployment builder regardless. |

## Sign-offs

- [x] Author: GitHub Copilot CLI agent (claude-opus-4.7).
- [x] Reviewer: Pal Lakatos-Toth (admin merge).

## §14.6 column-11 status after this slice

§14.6 column 11 (Multi-runtime hosting) **flipped to ✓** with S10.A4 (PR #70) — ≥2 native non-OpenClaw runtimes (OpenAI Agents Python + MAF Python) shipped end-to-end, plus BYO with documented contract. S10.A5 makes the value prop **operator-accessible**: a customer can now `azureclaw add --runtime microsoft-agent-framework my-maf-agent` and it works without reading the CRD by hand.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
