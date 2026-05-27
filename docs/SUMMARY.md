# Summary

[Introduction](README.md)

# Getting started

- [Getting started](getting-started.md)
- [CLI reference](cli-reference.md)
- [Use cases](use-cases.md)
  - [Exec-brief walkthrough](use-cases/exec-brief-walkthrough.md)

# Architecture

- [Architecture overview](architecture.md)
- [Architecture diagrams](architecture-diagrams.md)
- [Runtimes](runtimes.md)
- [A2A gateway (architecture)](architecture/a2a-gateway.md)
- [AGT boundary](architecture/agt-boundary.md)
- [Multi-tenant model](multi-tenant.md)
- [Egress proxy](egress-proxy.md)

# Security

- [Security overview](security.md)
- [STRIDE × trust boundaries](security/stride.md)
- [Red team playbook](security/red-team.md)
- [CRD trust model](security/crd-trust-model.md)
- [Security validation](security-validation.md)
- [MCP top-10](security-mcp-top10.md)
- [Upstream alignment](upstream-alignment.md)

# Agent capabilities

- [Channels & plugins](channels-plugins.md)
- [Operator TUI](operator-tui.md)
- [Permissions model](permissions.md)
- [Per-sandbox identity (Entra Agent ID)](agent-identity.md)
- [Demo script](demo-script.md)
- [Examples catalogue](examples.md)

# Operations

- [Operations overview](operations/README.md)
- [A2A gateway (operations)](operations/a2a-gateway.md)
- [BYO strict mode](operations/byo-strict.md)
- [Branch protection](operations/branch-protection.md)
- [Chaos tier](operations/chaos-tier.md)
- [GitOps](operations/gitops.md)
- [Helm packaging](operations/helm-packaging.md)
- [Image versioning](operations/image-versioning.md)
- [Secret rotation](operations/secret-rotation.md)
- [Supply chain](operations/supply-chain.md)

# API & policy

- [CRD reference](api/crd-reference.md)
- [KarsEval (operator guide)](api/karseval.md)
- [Lifecycle & reconciliation](api/lifecycle.md)
- [Conditions](api/conditions.md)
- [Policy canonical format](api/policy-canonical-format.md)

# Blueprints

- [Index](blueprints/00-index.md)
- [Developer inner loop](blueprints/01-developer-inner-loop.md)
- [Local k8s dev loop](blueprints/02-local-k8s-dev-loop.md)
- [Enterprise self-hosted](blueprints/02-enterprise-self-hosted.md)
- [Managed public offload](blueprints/03-managed-public-offload.md)
- [Cross-org federation](blueprints/04-cross-org-federation.md)
- [Sovereign / air-gapped](blueprints/05-sovereign-airgapped.md)

# Roadmap & ADRs

- [Roadmap](roadmap.md)
- [ADR index](adr/README.md)
  - [ADR-0001: A2A ingress front edge](adr/0001-a2a-ingress-front-edge.md)
  - [ADR-0002: Inference endpoint sourcing](adr/0002-inference-endpoint-sourcing.md)

