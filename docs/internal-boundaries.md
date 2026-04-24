# Internal MSFT Product Boundaries

**Status:** internal. Not published externally.
**Companion:** `docs/implementation-plan.md` §3 is the canonical version; this
file is the standalone reference.

AzureClaw does not compete with other Microsoft products. Every overlap with
a neighbouring MSFT product resolves to one of three postures:

- **Consume** — we call their API / include their capability.
- **Be consumed** — they call ours / embed ours.
- **Orthogonal** — explicitly non-overlapping scope.

A new CRD or capability in AzureClaw MUST be added to the table below with a
declared posture before it merges. `ci/security-audit-required.sh` checks for
the posture reference in the relevant security-audit doc.

---

## Boundary matrix

| MSFT product | Overlap surface | Posture | How we stay separate |
|---|---|---|---|
| **Azure AI Foundry** | Model serving, agent orchestration, Memory Store, Evals | **Consume** | `ClawMemory` is a Foundry Memory Store *binding* CR, never an in-cluster memory backend. `ClawEval` integrates with Foundry Evals + Promptflow. `InferencePolicy` is a budget/guardrail CR, not a model-router. Router calls Foundry, never replaces it. |
| **KAITO** | Model deployment on K8s | **Orthogonal** | KAITO deploys models (payload). AzureClaw deploys *agents that consume models*. A KAITO inference workspace can be the target of an AzureClaw `InferencePolicy`. |
| **Azure Container Apps Dynamic Sessions** | Code-exec sandboxes | **Orthogonal** | ACA is serverless one-shot exec. AzureClaw is a full agent runtime on AKS with mesh, governance, persistent identity. ACA can be a *target* tool via `McpServer`. |
| **AKS-core (Istio / Gateway API / Workload Identity)** | K8s primitives AzureClaw uses | **Consume** | We use Gateway API for ingress, Workload Identity for federated auth, native K8s NetworkPolicy. We do not ship a fork of any of these. |
| **Entra ID / Workload Identity Federation** | Identity | **Consume** | `ClawAgentIdentity` (Phase 4) federates SPIFFE SVIDs through Entra WIF; we never reimplement identity. |
| **Microsoft Defender for Cloud** | Cloud security posture | **Orthogonal** | Defender reports on cluster-level posture. AzureClaw's audit chain is agent-operation-scoped. They nest naturally. |
| **Azure Policy / OPA Gatekeeper** | Admission | **Complementary** | Azure Policy handles org-wide AKS policy. AzureClaw ships agent-scoped VAP/MAP. Both run; not a replacement. |
| **Microsoft Sentinel** | SIEM | **Be consumed** | AGT audit receipts feed Sentinel via OTel. Sentinel consumes; we never replace. |
| **Microsoft 365 Agent Framework / Copilot Studio** | Agent authoring | **Orthogonal + partial consume** | M365/Copilot Studio authors agents at the SaaS layer; AzureClaw is the AKS-hosted runtime for developer-authored agents (via OpenClaw, Claude Agent SDK, OpenAI SDK, etc.). Copilot Studio agents can invoke AzureClaw-hosted MCP servers. |
| **Microsoft Intune / Purview** | Compliance | **Be consumed** | Audit chain is queryable by compliance tools. |

---

## Per-CRD posture (added as CRDs land)

| CRD | Posture vs Foundry / nearest neighbour | Justification |
|---|---|---|
| `ClawSandbox` | **Orthogonal** to Foundry agent service | Foundry orchestrates agent *behaviours*; `ClawSandbox` is the AKS *runtime substrate*. Foundry agents can deploy onto a `ClawSandbox`. |
| `McpServer` | **Orthogonal** to Foundry MCP hosting | Foundry hosts managed/SaaS MCP servers. `McpServer` is for AKS-hosted private/custom tool servers (company-internal APIs, VPC-restricted tools). They co-exist; one tenant can use both. |
| `ToolPolicy` | **Complementary** to Foundry guardrails | Foundry Content Safety stays the model-side filter. `ToolPolicy` is sandbox-side per-tool gating (rate limit, AP2 caps, approval). Both apply; neither replaces the other. AGT `PolicyEngine` is the verdict engine for `ToolPolicy`; Foundry does not police tool calls. |
| `ClawSandbox.spec.a2a` | **Orthogonal** to Foundry A2A (native) | Foundry hosts its managed agents over A2A. `ClawSandbox.spec.a2a` exposes AKS-hosted agents over A2A 1.0.0. Interop is the spec. They never publish the same agent. |

---

## Rule for new CRDs

Every new CRD merges only if:

1. It has a row in the matrix above.
2. Posture is one of `Consume`, `Be consumed`, or `Orthogonal`.
3. The security-audit doc for the CRD cites this row.
4. If the posture is `Consume` against a partner team, we have a written
   statement from that team. `ClawMemory` (Phase 2) is the first such case —
   see `docs/implementation-plan.md` §14 open decision #5.

## Rule for CLI commands

New CLI commands do not need a matrix row unless they introduce a new
integration surface (e.g., a new channel plugin). Channel plugins go under
`docs/internal-boundaries-channels.md` (Phase 1, when we add the first
non-Telegram channel).

## Non-goals

- This file is not a competitive positioning document. That lives in
  `docs/competitive.md` (gitignored) for external/competitive landscape,
  not for internal MSFT relationships.
