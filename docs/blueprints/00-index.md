# Deployment blueprints

Five concrete shapes for running Kars. Each blueprint pins down **who runs what**, **where the trust boundary sits**, and **the main flow** end to end.

These are not mutually exclusive — one Kars cluster can serve several of them simultaneously (e.g. Blueprint 02 for internal employees + Blueprint 03 for external partners on the same cluster).

## Catalogue

| # | Blueprint | Audience | Where Kars runs | Status |
|---|---|---|---|---|
| **[01](01-developer-inner-loop.md)** | Developer inner loop | Individual contributor | Laptop (`kars dev` — single Docker container) | ✅ |
| **[02](02-local-k8s-dev-loop.md)** | Local Kubernetes dev loop | Maintainer / agent author | Laptop (`kars dev --target local-k8s` — kind + Helm + Headlamp) | ✅ |
| **[03](02-enterprise-self-hosted.md)** | Enterprise self-hosted | Platform team, single org | Customer-owned AKS, single tenant | ✅ |
| **[04](03-managed-public-offload.md)** | Managed public offload | SaaS provider, many tenants | Provider-owned AKS, multi-tenant, optional Kata + AMD SEV-SNP | ✅ runtime · 🚧 productization |
| **[05](04-cross-org-federation.md)** | Cross-org federation | Two or more orgs collaborating | Two AKS clusters, mesh + A2A across | ✅ |
| **[06](05-sovereign-airgapped.md)** | Sovereign / air-gapped | Regulated / classified / disconnected | Isolated AKS, no public egress | 🚧 patterns documented |

## How to read each blueprint

Every blueprint follows the same shape:

1. **Persona & intent** — who, what, why.
2. **Topology** — Mermaid diagram of who runs what.
3. **Trust boundary** — where the credential / control boundary sits.
4. **Primary flow** — sequence diagram of the main happy-path interaction.
5. **What you provision** — concrete CLI / Helm / `kubectl` invocations.
6. **What is unique** — the property that makes this blueprint not just a sub-case of another.
7. **References** — code, CRDs, related docs.

## What every blueprint inherits

These properties are not blueprint-specific; they come from running Kars at all. If your environment cannot satisfy them, you are outside the threat model — open an issue before deploying.

- **Egress isolation.** Agent runs as UID 1000 with no path to the network. The router (UID 1001) is the only egress. Enforced by the `egress-guard` initContainer (iptables) and a Kubernetes NetworkPolicy.
- **Foundry-side Content Safety.** `Microsoft.DefaultV2` Prompt Shields on every inference, both directions.
- **AGT governance.** `PolicyEngine`, `TrustManager`, `AuditLogger`, `RateLimiter`, `BehaviorMonitor` evaluated in-process on every tool call, every inference, every mesh message.
- **Tamper-evident audit.** Hash-chained log via `AuditSink`. Each record is signed.
- **Signal-Protocol mesh.** X3DH + Double Ratchet. Relay sees only ciphertext. Failed decrypt is a `security_event`; there is no plaintext fallback.
- **CRD-driven control plane.** Eight CRDs in `kars.azure.com/v1alpha1`: `KarsSandbox`, `A2AAgent`, `McpServer`, `ToolPolicy`, `InferencePolicy`, `KarsMemory`, `KarsEval`, `TrustGraph`. Full schema in [`docs/api/crd-reference.md`](../api/crd-reference.md).
- **Multi-runtime hosting.** `KarsSandbox.spec.runtime.kind` selects the runtime: `OpenClaw` (default), `OpenAIAgents`, `MicrosoftAgentFramework` (Python — .NET deferred), `LangGraph` (Python or TypeScript), `Anthropic`, `PydanticAi`, or `BYO`. `SemanticKernel` is reserved but not yet wired. See [Runtime catalog](../runtimes.md).
- **InferencePolicy reference.** Sandboxes bind to an `InferencePolicy` by name; model and budget configuration is no longer inline.

---

The blueprints reuse the diagrams in [Architecture diagrams](../architecture-diagrams.md) where possible — go there for the canonical view of any given component.
