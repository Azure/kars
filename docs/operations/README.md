# Operations

How to operate AzureClaw in production. Each page is one operational concern, with the full runbook for that concern.

| Topic | Read |
|---|---|
| **GitOps** — managing AzureClaw with Flux / Argo CD instead of the CLI. | [`gitops.md`](gitops.md) |
| **Secret rotation** — rotating Foundry keys, ACR credentials, federated identities. | [`secret-rotation.md`](secret-rotation.md) |
| **Image versioning** — `:latest` tag policy, pinning by digest, supply-chain considerations. | [`image-versioning.md`](image-versioning.md) |
| **Helm packaging** — packaging the chart for offline / sovereign deployments. | [`helm-packaging.md`](helm-packaging.md) |
| **Branch protection** — repository hygiene for forks and downstream consumers. | [`branch-protection.md`](branch-protection.md) |
| **Supply chain** — Cosign signing, SBOM, SLSA provenance. | [`supply-chain.md`](supply-chain.md) |
| **Chaos tier** — fault-injection harness used in CI and on demand against staging. | [`chaos-tier.md`](chaos-tier.md) |
| **A2A gateway** — operating the public-ingress A2A endpoint. | [`a2a-gateway.md`](a2a-gateway.md) |
| **BYO strict** — running the strict-validation BYO contract. | [`byo-strict.md`](byo-strict.md) |

## Day-to-day reference

For day-to-day work the most useful surfaces are:

- **`azureclaw operator`** — the live fleet TUI. See [`docs/operator-tui.md`](../operator-tui.md).
- **`azureclaw status <name>`** — quick health snapshot of one sandbox.
- **`azureclaw logs <name> -f`** — tail router + agent logs.
- **`azureclaw policy show <name>`** — what is allowed / denied / approval-gated for a sandbox.
- **`kubectl describe clawsandbox <name>`** — full condition chain (every status condition is documented in [`../api/conditions.md`](../api/conditions.md)).
- **`azureclaw eval`** — reproducible evaluation against a pinned sandbox spec.

## What is not here

- **Cluster provisioning** — see [Getting started](../getting-started.md). `azureclaw up` provisions AKS, ACR, Foundry, federated identity, and Helm install in one go.
- **Architecture and CRDs** — see [Architecture](../architecture.md) and [CRD reference](../api/crd-reference.md).
- **Security guarantees** — see [Security model](../security.md).

## See also

- [CLI reference](../cli-reference.md)
- [Backwards compatibility](../api/backwards-compatibility.md)
- [Conditions reference](../api/conditions.md)
