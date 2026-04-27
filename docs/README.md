# AzureClaw Documentation

## Getting Started

- [Quick Start](../README.md#quick-start) — Install, deploy, connect in 5 minutes
- [Use Cases](use-cases.md) — Three canonical scenarios with code citations
- [Phase 0 + 1 Capability Index](phase-0-1-capabilities.md) — Evidence-based manifest for PR #44
- [Architecture](architecture.md) — Components, CRD schema, four-seam providers, MCP/A2A modules, API endpoints
- [Architecture Diagrams](architecture-diagrams.md) — Mermaid flow diagrams

## Security

- [Security Model](security.md) — Defense-in-depth (infra + AGT governance + E2E mesh + protocol-layer controls)
- [Threat Model — Routes](threat-model.md) — Per-route auth tier, input validation, blast-radius
- [AGT Vendored-Patch Audit](agt-vendored-patch-audit.md) — Vendored AgentMesh fixes pending AGT mesh shipping
- [`sigs/agent-sandbox` Compat](sigs-agent-sandbox-compat.md) — Optional Translate / Overlay mode design
- [OWASP MCP Top 10 (2025)](security-mcp-top10.md) — Controls matrix for the new MCP 2026 surface
- [ADR-0001 — A2A ingress front-edge](adr/0001-a2a-ingress-front-edge.md) — Gateway-only, surgical opt-in
- [Security Reviewers Roster](security-reviewers.md) — Reviewer assignment + SLA for security-audit sign-off
- [Security Audits](security-audits/) — 75 per-capability audit docs (Phase 0 + Phase 1)
- [Network Egress & Proxy](egress-proxy.md) — Blocklist, allowlist, approval flow, learn mode
- [E2E Encryption Proof](e2e-encryption-proof.md) — Signal Protocol inter-agent messaging with traffic capture evidence
- [Security Validation](security-validation.md) — Live cluster evidence for every security layer
- [Permissions](permissions.md) — Required Azure RBAC for `azureclaw up`
- [Upstream Alignment](upstream-alignment.md) — How AzureClaw extends OpenClaw via upstream extension points (no fork)

## Agent Capabilities

- [Channels & Plugins](channels-plugins.md) — Telegram, Slack, Discord, WhatsApp, search plugins, Foundry Bing Grounding
- [Architecture — Foundry Integration](architecture.md#foundry-standalone-apis-18-api-groups-imds-auth) — Responses API, Memory Store, Foundry IQ
- [Any-OpenClaw + Cloud Offload](any-openclaw-cloud-offload.md) — Run the `azureclaw-mesh` plugin in *any* OpenClaw host (NemoClaw, laptop, …) and offload tasks to AzureClaw sandboxes over Signal E2E

## Operations

- [Egress Management](egress-proxy.md#operator-workflow) — Learn → review → approve → lock down
- [Agent Handoff](architecture.md#cloud-handoff) — Live migration between local Docker and AKS
- [Multi-Tenant Isolation](multi-tenant.md) — Per-namespace security boundaries

## Demos & Examples

- [ClawShield Demo](DEMO.md) — Full walkthrough with screenshots
- [Example Agents](../examples/) — `basic-agent`, `confidential-agent`, `demo-clawshield`

## Migration

- [Migration from NemoClaw](migration-from-nemoclaw.md) — What changed and how to migrate
