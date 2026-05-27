# Kars documentation

A secure runtime for AI agents on Azure Kubernetes Service. This is the documentation index. The top-level [`README`](../README.md) is a faster on-ramp; come here when you need depth.

## Choose your path

### Read in order if you are new
1. [Getting started](getting-started.md) — laptop in five minutes, then AKS.
2. [Architecture](architecture.md) — the design and why.
3. [Architecture diagrams](architecture-diagrams.md) — every component, dev and prod side by side.
4. [Use cases](use-cases.md) — the four scenarios Kars was built for.

### By audience

| You are a… | Start here |
|---|---|
| **Executive / decision-maker** | [Architecture](architecture.md) → [Blueprints](blueprints/00-index.md) → [Use cases](use-cases.md) |
| **Platform engineer** | [Getting started](getting-started.md) → [Operations](operations/) → [CLI reference](cli-reference.md) |
| **Security engineer** | [Security model](security.md) → [STRIDE](security/stride.md) → [Red-team playbook](security/red-team.md) → [MCP top-10](security-mcp-top10.md) |
| **Agent builder** | [Runtimes](runtimes.md) → [CRD reference](api/crd-reference.md) → [CLI reference](cli-reference.md) |
| **Site reliability** | [Operations / GitOps](operations/gitops.md) → [Conditions](api/conditions.md) → [Egress proxy](egress-proxy.md) |

## Reference

### Architecture & design
- [Architecture](architecture.md) — the canonical design doc.
- [Architecture diagrams](architecture-diagrams.md) — dev, prod, mesh, A2A, MCP.
- [A2A gateway](architecture/a2a-gateway.md) — public-ingress topology and trust model.
- [AGT boundary](architecture/agt-boundary.md) — what AGT enforces vs what Kars enforces.
### API
- [CRD reference](api/crd-reference.md) — all nine CRDs with schema and examples.
- [Conditions reference](api/conditions.md) — every status condition the controller emits.
- [Policy canonical format](api/policy-canonical-format.md) — signing canonicalization rules.

### Runtimes
- [Runtime catalog](runtimes.md) — seven first-class adapters and the BYO contract.

### Blueprints
- [Index](blueprints/00-index.md)
- [01 — Developer inner loop](blueprints/01-developer-inner-loop.md)
- [02 — Enterprise self-hosted](blueprints/02-enterprise-self-hosted.md)
- [03 — Managed public offload](blueprints/03-managed-public-offload.md)
- [04 — Cross-org federation](blueprints/04-cross-org-federation.md)
- [05 — Sovereign / air-gapped](blueprints/05-sovereign-airgapped.md)

### Security
- [Security model](security.md) — the layered control plane.
- [STRIDE](security/stride.md) — threat model.
- [Red-team playbook](security/red-team.md) — adversarial scenarios.
- [Security validation](security-validation.md) — what CI verifies.
- [MCP top-10](security-mcp-top10.md) — how Kars addresses each item.
- [Upstream alignment](upstream-alignment.md) — the OpenClaw extension contract.

### Operations
- [Operations index](operations/) — fleet operations, GitOps, upgrades.
- [Operator TUI](operator-tui.md) — `kars operator`.
- [Egress proxy](egress-proxy.md) — outbound network controls.

### CLI
- [CLI reference](cli-reference.md) — every command, every flag.

## What is **not** here

`docs/internal/` holds historical phase audits, migration logs, and one-off proofs that exist for traceability but are not part of the public surface. They are excluded from the rendered site.

## Reading the site offline

```bash
make docs-site-serve   # serves at http://localhost:3000
make docs-site         # builds to target/book/index.html
```

The site is built with [mdBook](https://rust-lang.github.io/mdBook/). The chapter index is **[`SUMMARY.md`](SUMMARY.md)**.
