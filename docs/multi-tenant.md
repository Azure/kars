# Multi-Tenant Namespace Isolation

## How AzureClaw isolates tenants

Each sandbox gets its own Kubernetes namespace with full isolation:

### Namespace per sandbox
```
azureclaw-system          # Controller, CRDs, RBAC
azureclaw-agent-1         # Tenant A's sandbox
azureclaw-agent-2         # Tenant B's sandbox
azureclaw-agent-3         # Tenant C's sandbox
```

### Security boundaries per namespace

| Layer | How it works |
|---|---|
| **Pod Security** | `baseline` enforcement (allows egress-guard init container), `restricted` audit/warn |
| **Network** | Default-deny egress NetworkPolicy per namespace |
| **RBAC** | ServiceAccount per namespace with minimal permissions |
| **Seccomp** | Custom seccomp profile (enhanced) or RuntimeDefault (standard) |
| **Kata VM** | Full VM boundary per pod (confidential) |
| **Resource limits** | CPU/memory limits per sandbox pod |
| **Token budgets** | Per-sandbox daily token limits in inference router |
| **Workload Identity** | Per-sandbox federated credential + ServiceAccount |

### What tenants CANNOT do
- Access other tenants' namespaces
- Read other tenants' secrets or config
- Communicate with other tenants' pods (NetworkPolicy blocks inter-namespace traffic)
- Escape the container (seccomp + read-only rootfs + non-root + drop ALL capabilities)
- Access IMDS from the agent container (blocked by iptables UID-based rules)

### Creating multiple tenants
```bash
# Each azureclaw up creates an isolated sandbox
azureclaw up --name tenant-a --isolation enhanced --model gpt-4.1
azureclaw up --name tenant-b --isolation confidential --model Phi-4
azureclaw up --name tenant-c --isolation standard --model gpt-4o

# Each gets its own namespace, NetworkPolicy, ServiceAccount, and federated credential
kubectl get clawsandbox -n azureclaw-system
NAME       PHASE     MODEL     ISOLATION
tenant-a   Running   gpt-4.1   enhanced
tenant-b   Running   Phi-4     confidential
tenant-c   Running   gpt-4o    standard
```
