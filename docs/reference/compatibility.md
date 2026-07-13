# Compatibility and support matrix

This page separates **template portability** from **tested product support**.

## Tested environments

| Environment | Status | What is covered |
|---|---|---|
| Local kind | Tested development path | CRDs, controller, sandbox pod shape, router, NetworkPolicy, seccomp, OpenClaw/Hermes basics |
| AKS | Primary tested deployment | Workload Identity, Azure inference, H100 local inference, managed MCP, encrypted mesh, missions/teams, governance |
| EKS | Helm-renderable, not live-qualified | Operator must provide registry, identity, inference, CNI, ingress, storage, and mesh integration |
| GKE | Helm-renderable, not live-qualified | Operator must provide registry, identity, inference, CNI, ingress, storage, and mesh integration |
| Other conformant Kubernetes | Experimental | No blanket support claim |

## Kubernetes requirements

- Kubernetes **1.30+** for the enabled ValidatingAdmissionPolicy controls.
- `admission.seccompAutoStamp` additionally requires Kubernetes 1.34 and the
  beta `MutatingAdmissionPolicy` feature gate; it is disabled by default.
- A CNI that enforces Kubernetes `NetworkPolicy`.
- A runtime that supports `RuntimeDefault` seccomp; the optional custom
  `kars-strict` profile requires the seccomp installer or equivalent node setup.
- Cluster permissions to install CRDs, cluster-scoped RBAC, admission policies,
  and optional DaemonSets.

## External dependencies

The core Helm chart does not make every dependency disappear. A complete
deployment needs:

- pull access to controller, router, runtime, and managed-MCP images;
- an inference backend and authentication path;
- Microsoft AGT AgentMesh relay and registry;
- DNS and NetworkPolicy behavior compatible with the selected CNI;
- optional cert-manager/TLS components for public A2A;
- optional AI Runway/KAITO for in-cluster model deployments;
- optional monitoring backends.

## Runtime capability matrix

See [Runtimes](../runtimes.md). An adapter marked as shipping is not
automatically equivalent to OpenClaw or Hermes for MCP, mesh, spawn, channels,
artifacts, or deep E2E coverage.

## Versioning

Until release automation unifies package, chart, and image versions, treat the
Git commit and image digests as the compatibility authority. Do not infer
compatibility from `Chart.yaml` alone.

Public releases must publish a table mapping the Azure/kars tag or full commit
to CLI, chart, controller, router, runtime, and managed-MCP image digests. A
private feature branch or one-off acceptance environment is not a public
compatibility authority.
