# Kars documentation

Kars is a Kubernetes-native runtime for isolated, governed AI agents. These
docs distinguish:

- **tutorials**: learn by completing an end-to-end journey;
- **how-to guides**: accomplish one operational task;
- **concepts**: understand architecture, security, and trade-offs;
- **reference**: exact APIs, configuration, compatibility, and conditions.

## Choose your path

| You want to… | Start here |
|---|---|
| Run Kars locally | [Quickstart](quickstart.md) |
| Deploy to AKS | [Getting started](getting-started.md) |
| Install with Helm on an existing cluster | [Helm installation](how-to/helm-installation.md) |
| Add Playwright or another MCP | [Managed MCP tutorial](tutorials/managed-mcp.md) |
| Understand Kars versus Kars Bridge | [Product boundary](concepts/kars-and-bridge.md) |
| Understand durable team runs, checkpoints, review, and memory | [Durable team workflows](concepts/durable-team-workflows.md) |
| Assess platform support | [Compatibility matrix](reference/compatibility.md) |
| Debug a failure | [Troubleshooting](operations/troubleshooting.md) |
| Review the security model | [Security](security.md) |
| Implement a runtime adapter | [Runtime contract](runtimes/CONTRACT.md) |

## What Kars guarantees

Kars moves credentials and external network access out of the agent process and
into a separate per-pod router. The exact guarantee depends on the deployment:

- kind validates the Kubernetes pod, policy, and UID boundary locally;
- AKS adds Workload Identity, Azure model services, and optional confidential
  nodes;
- strict egress blocks unapproved destinations, while learning mode observes
  and brokers requests differently;
- anonymous mesh and Entra-verified mesh provide different identity assurance.

Use [feature maturity](maturity.md), [security](security.md), and
[compatibility](reference/compatibility.md) together when evaluating production
readiness.

## Documentation map

### Learn

- [Quickstart](quickstart.md)
- [Full getting started](getting-started.md)
- [Managed MCP tutorial](tutorials/managed-mcp.md)
- [Examples](examples.md)
- [Use cases](use-cases.md)

### Understand

- [Architecture](architecture.md)
- [Architecture diagrams](architecture-diagrams.md)
- [Kars and Kars Bridge](concepts/kars-and-bridge.md)
- [Durable team workflows](concepts/durable-team-workflows.md)
- [Runtimes](runtimes.md)
- [MCP](mcp.md)
- [AgentMesh and AGT boundary](architecture/agt-boundary.md)
- [Multi-tenant model](multi-tenant.md)

### Operate

- [Operations overview](operations/README.md)
- [Troubleshooting](operations/troubleshooting.md)
- [Upgrades and rollback](operations/upgrades.md)
- [Secret rotation](operations/secret-rotation.md)
- [GitOps](operations/gitops.md)
- [Supply chain](operations/supply-chain.md)

### Secure

- [Security model](security.md)
- [CRD trust model](security/crd-trust-model.md)
- [STRIDE analysis](security/stride.md)
- [MCP security top 10](security-mcp-top10.md)
- [Security validation](security-validation.md)

### Reference

- [Compatibility matrix](reference/compatibility.md)
- [CRD reference](api/crd-reference.md)
- [Conditions](api/conditions.md)
- [Lifecycle](api/lifecycle.md)
- [CLI reference](cli-reference.md)
- [Runtime contract](runtimes/CONTRACT.md)

## Site and contribution

`SUMMARY.md` is the canonical mdBook navigation.

```bash
make docs-site
make docs-site-serve
```

For documentation standards, page types, commands, and link conventions, see
[Contributing documentation](contributing/documentation.md).
