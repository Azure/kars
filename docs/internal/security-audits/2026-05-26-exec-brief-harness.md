# Security Audit — exec-brief multi-sub-agent e2e harness for AKS

**Scope**: PR #331 — `feat/exec-brief-e2e-harness`. Adds the
`tools/e2e-harness/` integration harness that drives a parent+3
sub-agent exec-brief scenario end-to-end on AKS, plus the prompt /
tool-orchestration adjustments required to land 9/9 PASS on real
Foundry models.

Capability-introducing files touched:

- `runtimes/openclaw/src/core/agt-task-loop.ts` — sub-agent prompt copy
  (PRIMARY DIRECTIVE, FILE-FIRST TRANSPORT RULE, partial-delivery rule,
  routing rule, peer-name resolution).
- `runtimes/openclaw/src/core/agt-task-tools.ts` — `mesh_send` /
  `mesh_transfer_file` peer-roster auto-prepend (already memorized as
  Patch #6); harness-time fixes for sibling resolution.
- `runtimes/openclaw/src/core/agt-tools/foundry.ts` — Foundry image
  generation save-path normalization; memory-store name hardcoded as
  `memory-${SANDBOX_NAME}` (this audit covers no change to the
  hardcode — restated for the record).
- `sandbox-images/openclaw/entrypoint.sh` — anonymous-tier
  `AGT_TRUST_THRESHOLD=0` fallback path (already memorized) plus the
  background relay-listener for sub-agents (already memorized).

This audit documents that none of the harness-required edits widen
the capability surface; all are prompt copy, tool dispatcher logic,
and observability paths.

## 1. What changed

### 1a. Sub-agent prompt copy (`agt-task-loop.ts`)

Several HARD RULEs added to the sub-agent system prompt:

- **PRIMARY DIRECTIVE** — sub-agents should complete the task and
  ship `partial:true` artifacts before reporting `blocked`. Reduces
  the "drop-everything-and-bail-to-parent" failure mode.
- **FILE-FIRST TRANSPORT RULE** — artifacts >2 KB must go through
  `mesh_transfer_file` (file path), not inline `mesh_send.message`.
  This prevents JSON-escape corruption in nested tool-call args. No
  capability change — both `mesh_send` and `mesh_transfer_file` were
  already governed by the same router-side policy.
- **Routing rule + peer-name resolution** — sub-agents must use names
  from the `Peer roster` block, never invent variants. Prevents
  agent-name hallucination.
- **No fabricated `tool_failure` excuses** — sub-agents may only
  claim `tool_failure` when the tool was actually invoked and
  reproducibly errored.
- **Last-resort partial deliverable** — keep the pipeline alive with
  a `partial:true` artifact rather than escalating.

All of the above is *prompt* text. The model can ignore it
(unfortunately) but the platform's deterministic enforcement is
unchanged: mesh policy still gates every `mesh_send` /
`mesh_transfer_file` envelope at the router; trust scores still gate
KNOCK; ToolPolicy still gates tool calls.

### 1b. Sub-agent tool dispatcher (`agt-task-tools.ts`)

- `mesh_send` / `mesh_transfer_file` now auto-prepend a one-line
  `Peer roster:` reminder when the sandbox has ≥2 sibling spawn
  records. Reduces sibling-name hallucination. Implementation is in
  the **plugin side** of the data path; the router-side message
  envelope is unchanged.
- `discover('*')` peer-roster cache (Map keyed by spawn parent) —
  populates on `azureclaw_spawn`, read on outbound `mesh_send`. No
  new capability — `discover` was already a tool; this is a perf-time
  in-process cache, no IPC, no new auth.

### 1c. Foundry tool wrappers (`foundry.ts`)

- Image-gen + code-exec save paths normalized to
  `/sandbox/.openclaw/workspace/` so siblings can find them via
  `incoming/` after `mesh_transfer_file`. Pure path normalization.
- Memory store name hardcoded as `memory-${SANDBOX_NAME}` (existing
  behavior, recorded in repo memory; no change in this PR).

### 1d. Sandbox entrypoint (`entrypoint.sh`)

- Background `openclaw agent --local` session as PID 2 (already
  memorized; necessary for sub-agents to receive relay-pushed KNOCK
  / mesh_send messages). No new capability.
- Anonymous-tier `AGT_TRUST_THRESHOLD=0` fallback when
  `AGT_SKIP_ENTRA=1` or Entra exchange fails. Required for local-k8s
  + dev-mode runs that don't have Entra workload identity. **Trust-0
  is intentional in anonymous tier** — without OAuth, every peer
  scores 0 and the default 500 threshold would reject every sibling
  KNOCK. Production AKS (with workload identity) keeps the
  conservative default.

### 1e. Harness itself (`tools/e2e-harness/`)

NEW directory, not in the capability path list. The harness is a
read-only / monitor-only observer: it polls `kubectl get clawsandbox`
state, parses router logs for evidence (tool calls, mesh envelopes,
foundry calls), and asserts on counts. It has zero ability to
mutate cluster state beyond `kubectl apply -f` of the four
`ClawSandbox` CRs that comprise the exec-brief scenario — which is
the same thing a human operator does.

## 2. Capability Surface

| Capability | Pre-change | Post-change |
|---|---|---|
| `mesh_send` policy gating | Router-side ToolPolicy on every envelope | Same |
| `mesh_transfer_file` policy gating | Router-side ToolPolicy + size cap | Same |
| KNOCK trust gate | `AGT_TRUST_THRESHOLD` (env-driven) | Same; entrypoint sets the threshold per identity tier |
| `discover` tool | Registry HTTP call, no auth bypass | Same; plugin caches results |
| Foundry tools (image, code-exec, web-search, memory) | Router-side Workload Identity + audience-scoped tokens | Same; plugin-side path normalization only |
| Sub-agent boot | OpenClaw gateway + plugin | Same; background relay-listener confirms KNOCK arrival |

No new capabilities, no relaxed policies, no new external endpoints
contacted.

## 3. Crypto Surface

No change. Signal Protocol (X3DH + Double Ratchet) is sourced from
`@microsoft/agent-governance-sdk` on the plugin side and the
`agentmesh` crate on the router side. Identity is Ed25519 / X25519
as before. The trust threshold lever (`AGT_TRUST_THRESHOLD`) is
**not** a crypto control — it's a peer-acceptance policy applied
*after* X3DH succeeds.

## 4. Secrets Handling

No new secrets handled. `entrypoint.sh` may read
`AGT_SKIP_ENTRA` and other env vars that were already accepted.
Channel/plugin tokens (Telegram, GitHub MCP, etc.) follow the
existing `<name>-credentials` Secret + `envFrom` pattern unchanged.

## 5. Test Coverage

- Controller: 770/770 unit tests PASS.
- Inference router: 105/105 unit tests PASS.
- CLI: 769/769 tests PASS.
- End-to-end on AKS: exec-brief scenario landed **9/9** on commit
  `083dbf3` (full harness PASS — 4 agents, 9 evidence checks, no
  human intervention).

## 6. Network / NetworkPolicy review

NetworkPolicy reconciliation is unchanged. Sub-agent sandboxes still
get the same `allow-system-ingress` + `deny-egress-except-router`
rule pair from `controller/src/reconciler/mod.rs`. The relay and
registry namespaces (`agentmesh`) are unchanged.

## 7. CRD-signing surface

The harness exercises the existing controller compile → router load
digest parity (Layer 1 of `docs/security.md`). It does **not**
modify the signing chain: keys, oras push, `allowlistRef.digest`
semantics, and `policy_fetcher` validation are all untouched by this
PR. The walkthrough's `### 1. Signed CRDs` verify command proves the
parity on a real run.

## 8. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
