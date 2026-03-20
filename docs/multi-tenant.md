# Multi-Tenant Namespace Isolation

Each sandbox runs in its own Kubernetes namespace with independent security boundaries. No shared state between tenants.

## Namespace Layout

```
azureclaw-system          # Controller (2 replicas), CRD, RBAC, seccomp DaemonSet
azureclaw-tenant-a        # Tenant A sandbox pod + NetworkPolicy + ServiceAccount
azureclaw-tenant-b        # Tenant B sandbox pod + NetworkPolicy + ServiceAccount
azureclaw-tenant-c        # Tenant C sandbox pod + NetworkPolicy + ServiceAccount
```

## Security Boundaries Per Namespace

| Layer | Enforcement |
|---|---|
| **PodSecurity** | `enforce: privileged` (egress-guard needs NET_ADMIN), `audit/warn: restricted` |
| **Network** | Default-deny egress NetworkPolicy. Per-sandbox allowlist via CRD. |
| **Per-container egress** | iptables: agent (UID 1000) → localhost + DNS only |
| **RBAC** | Dedicated ServiceAccount per namespace with WI annotation |
| **Seccomp** | Localhost `azureclaw-strict` (enhanced) or RuntimeDefault (standard) |
| **Kata VM** | Per-pod dedicated kernel (confidential level) |
| **Resource limits** | CPU/memory limits per sandbox pod |
| **Token budgets** | Per-sandbox daily + per-request limits in inference router |
| **Identity** | Per-sandbox federated credential + Managed Identity |

## Tenant Isolation Guarantees

- **No cross-namespace access** — NetworkPolicy blocks inter-namespace traffic
- **No shared secrets** — each namespace has its own ServiceAccount and config
- **No credential leakage** — agent (UID 1000) cannot reach IMDS; only the router (UID 1001) can
- **No container escape** — seccomp + read-only rootfs + non-root + drop ALL + optional Kata VM
- **No shared inference state** — each sandbox has its own router process with independent token tracking

## Usage

```bash
# First tenant deploys the full AKS stack
azureclaw up --name tenant-a --isolation enhanced --model gpt-4.1

# Subsequent tenants add sandboxes without redeploying infrastructure
azureclaw add tenant-b --isolation confidential --model Phi-4
azureclaw add tenant-c --isolation standard --model gpt-4o

kubectl get clawsandbox -n azureclaw-system
NAME       PHASE     MODEL     ISOLATION
tenant-a   Running   gpt-4.1   enhanced
tenant-b   Running   Phi-4     confidential
tenant-c   Running   gpt-4o    standard
```
