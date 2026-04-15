# AzureClaw Documentation

## Getting Started

- [Quick Start](../README.md#quick-start) — Install, deploy, connect in 5 minutes
- [Architecture](architecture.md) — System overview, components, CRD schema, API endpoints

## Security

- [Security Model](security.md) — 9-layer defense-in-depth (7 infra + AGT governance + E2E mesh)
- [Network Egress & Proxy](egress-proxy.md) — Blocklist, allowlist, approval flow, learn mode
- [E2E Encryption Proof](e2e-encryption-proof.md) — Signal Protocol inter-agent messaging with traffic capture evidence
- [Security Validation](security-validation.md) — Live cluster evidence for every security layer

## Agent Capabilities

- [Channels & Plugins](channels-plugins.md) — Telegram, Slack, Discord, WhatsApp, search plugins, Foundry Bing Grounding
- [Architecture — Foundry Integration](architecture.md#foundry-standalone-apis-18-api-groups-imds-auth) — Responses API, Memory Store, Foundry IQ

## Operations

- [Egress Management](egress-proxy.md#operator-workflow) — Learn → review → approve → lock down
- [Agent Handoff](architecture.md#cloud-handoff) — Live migration between local Docker and AKS
- [Multi-Tenant Isolation](multi-tenant.md) — Per-namespace security boundaries

## Demos & Examples

- [ClawShield Demo](DEMO.md) — Full walkthrough with screenshots
- [Example Agents](../examples/) — `basic-agent`, `confidential-agent`, `demo-clawshield`

## Migration

- [Migration from NemoClaw](migration-from-nemoclaw.md) — What changed and how to migrate
