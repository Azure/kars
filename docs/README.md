# AzureClaw Documentation

## Getting Started

- [Quick Start](../README.md#quick-start) — Install, deploy, connect in 5 minutes
- [Getting Started Guide](getting-started.md) — Walkthrough from `azureclaw up` to first agent
- [CLI Reference](cli-reference.md) — Every command and flag
- [Use Cases](use-cases.md) — Canonical scenarios with code citations
- [Architecture](architecture.md) — Components, CRD schema, four-seam providers, MCP/A2A modules, API endpoints
- [Architecture Diagrams](architecture-diagrams.md) — Mermaid flow diagrams
- [API & CRD Reference](api/crd-reference.md) — `ClawSandbox` schema (v1alpha1)

## Security

- [Security Model](security.md) — Defense-in-depth (infra + AGT governance + E2E mesh + protocol-layer controls)
- [Threat Model — Routes](threat-model.md) — Per-route auth tier, input validation, blast-radius
- [STRIDE Threat Model](security/stride.md) — Per-trust-boundary STRIDE matrix
- [AGT Boundary](architecture/agt-boundary.md) — What AzureClaw consumes vs builds, four provider contracts
- [AGT Vendored-Patch Audit](agt-vendored-patch-audit.md) — Vendored AgentMesh fixes pending AGT mesh shipping
- [`sigs/agent-sandbox` Compat](sigs-agent-sandbox-compat.md) — Optional Translate / Overlay mode design
- [OWASP MCP Top 10 (2025)](security-mcp-top10.md) — Controls matrix for the new MCP 2026 surface
- [ADR-0001 — A2A ingress front-edge](adr/0001-a2a-ingress-front-edge.md) — Gateway-only, surgical opt-in
- [Security Audits](security-audits/) — Per-capability audit docs (Phase 0 + Phase 1 + Phase 2)
- [Red-Team Findings Log](security/red-team.md) — Internal adversarial-test history
- [Network Egress & Proxy](egress-proxy.md) — Blocklist, allowlist, approval flow, learn mode
- [E2E Encryption Proof](e2e-encryption-proof.md) — Signal Protocol inter-agent messaging with traffic capture evidence
- [Security Validation](security-validation.md) — Live cluster evidence for every security layer
- [Permissions](permissions.md) — Required Azure RBAC for `azureclaw up`
- [Upstream Alignment](upstream-alignment.md) — How AzureClaw extends OpenClaw via upstream extension points (no fork)

## Agent Capabilities

- [Channels & Plugins](channels-plugins.md) — Telegram, Slack, Discord, WhatsApp, search plugins, Foundry Bing Grounding
- [Architecture — Foundry Integration](architecture.md#foundry-standalone-apis-18-api-groups-imds-auth) — Responses API, Memory Store, Foundry IQ
- [Any-OpenClaw + Cloud Offload](any-openclaw-cloud-offload.md) — Run the `azureclaw-mesh` plugin in *any* OpenClaw host (NemoClaw, laptop, …) and offload tasks to AzureClaw sandboxes over Signal E2E

## Architecture deep-dives

- [A2A Gateway](architecture/a2a-gateway.md) — Front-edge gateway design and component split
- [AGT Boundary](architecture/agt-boundary.md) — Responsibility split, provider contracts, outage modes
- [CRD Versioning Policy](architecture/crd-versioning.md) — `v1alpha1` freeze + `v1alpha2` + conversion-webhook plan
- [Backwards-Compatibility Commitment](api/backwards-compatibility.md) — SemVer surface, deprecation policy

## Operations

- [Egress Management](egress-proxy.md#operator-workflow) — Learn → review → approve → lock down
- [Agent Handoff](architecture.md#cloud-handoff) — Live migration between local Docker and AKS
- [Multi-Tenant Isolation](multi-tenant.md) — Per-namespace security boundaries
- [BYO Strict Mode](operations/byo-strict.md) — Validating-only admission for non-default runtimes
- [Branch Protection](operations/branch-protection.md) — Repo guardrails
- [Supply Chain](operations/supply-chain.md) — Cosign, SBOM, attestations
- [GitOps](operations/gitops.md) — Reconciler in a GitOps fleet
- [Image Versioning](operations/image-versioning.md) — Tag policy and runtime image overrides
- [Helm Packaging](operations/helm-packaging.md) — Chart versioning + local packaging
- [Secret Rotation Runbook](operations/secret-rotation.md) — Per-sandbox creds, TLS, AgentMesh identity, Azure
- [Chaos Tier](operations/chaos-tier.md) — Fault-injection test surface

## Roadmap

- [Roadmap](roadmap.md) — v1.0 capabilities, v1.1 targets, v1.2 directions, backlog

## Demos & Examples

- [ClawShield Demo](DEMO.md) — Full walkthrough with screenshots
- [Demo Guide](DEMO-GUIDE.md) — 30-minute first-launch → multi-agent flow
- [Demo Script](demo-script.md) — 3-act, ~15-minute live presentation
- [Example Agents](../examples/) — `basic-agent`, `confidential-agent`, `demo-clawshield`, `byo-quickstart`

## Migration

- [Migration from NemoClaw](migration-from-nemoclaw.md) — What changed and how to migrate
