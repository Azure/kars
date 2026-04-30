# AzureClaw Deployment Blueprints

Concrete, end-to-end shapes for running AzureClaw. Each blueprint pins down **who runs what**, **where the trust boundary sits**, and **how a single agent task flows from prompt to completion** — including which CRDs, controllers, network paths, and identity surfaces are in play.

The four [use cases in `docs/use-cases.md`](../use-cases.md) describe *what* AzureClaw does. The blueprints below describe *how you run it for a given audience*. They are not mutually exclusive — a single AzureClaw cluster can serve multiple blueprints simultaneously (e.g. Blueprint 02 for internal employees + Blueprint 03 for external partners).

## Blueprint catalogue

| # | Blueprint | Audience | Where AzureClaw runs | Today's status |
|---|---|---|---|---|
| **01** | [Developer inner-loop](01-developer-inner-loop.md) | Individual contributor on a laptop | Local Docker / kind, single-node | ✅ Shipping |
| **02** | [Enterprise self-hosted cluster](02-enterprise-self-hosted.md) | Platform team inside a single org | Customer-owned AKS, single tenant | ✅ Shipping |
| **03** | [Managed public offload service](03-managed-public-offload.md) | SaaS provider serving many external tenants | Provider-owned AKS, multi-tenant, Kata + AMD SEV-SNP | ✅ Runtime shipping · 🚧 SaaS productization in progress |
| **04** | [Cross-org federation](04-cross-org-federation.md) | Two or more orgs collaborating | Two AKS clusters meshed E2E | ✅ Shipping |
| **05** | [Sovereign / air-gapped](05-sovereign-airgapped.md) | Regulated, classified, disconnected, or sovereign-cloud workloads | Customer-owned AKS in an isolated network island | 🚧 Patterns documented; reproducible bundle on roadmap |

## Reading guide

Each blueprint has the same shape:

1. **Persona & intent** — who you are, what you want.
2. **Topology** — Mermaid diagram of who-runs-what.
3. **Trust boundary** — where the credential / control boundary sits.
4. **Primary flow** — sequence diagram of the main happy-path interaction.
5. **What you provision** — concrete CLI / kubectl invocations.
6. **What's unique to this blueprint** — the property that makes it not just a sub-case of another blueprint.
7. **References** — code, CRDs, ADRs.

## Cross-cutting properties

Regardless of blueprint, every AzureClaw deployment ships:

- **Egress isolation** — agent UID 1000 can only reach `localhost` + DNS. The router (UID 1001) is the sole external path.
- **Foundry-side Content Safety** — `Microsoft.DefaultV2` Prompt Shields on every inference.
- **AGT-native governance** — `PolicyEngine`, `TrustManager`, `AuditLogger`, `RateLimiter`, `BehaviorMonitor` evaluated in-process on every tool call, every inference, every mesh message.
- **Tamper-evident audit chain** — hash-chained log persisted via `AuditSink`.
- **Signal-Protocol mesh** — X3DH + Double Ratchet. Relay sees only ciphertext. No plaintext fallback; failed-decrypt is a `security_event`, never a delivered cleartext message.
- **CRD-driven control plane** — eight namespaced CRDs under `azureclaw.azure.com/v1alpha1`: `ClawSandbox`, `ClawPairing`, `McpServer`, `ToolPolicy`, `InferencePolicy`, `A2AAgent`, `ClawMemory`, `ClawEval`. All eight reconcilers shipped in Phase 2. Full reference: [`docs/api/crd-reference.md`](../api/crd-reference.md).
- **Multi-runtime hosting** — `spec.runtime.kind` selects the agent runtime variant: `OpenClaw` (default, Tier-1), `OpenAIAgents`, `MicrosoftAgentFramework` (Tier-1), `SemanticKernel`, `LangGraph`, `Anthropic` (Tier-2, schema shipped), or `BYO`. The inference-router, governance, and audit chain are runtime-agnostic.
- **`spec.inferenceRef.name` (ref form)** — sandboxes reference an `InferencePolicy` CR by name rather than inlining model/budget config. Inline inference fields were removed in S13; all example YAMLs in these blueprints use the ref form.

If your environment can't support one of those, you're outside the AzureClaw threat model — open an issue before deploying.
