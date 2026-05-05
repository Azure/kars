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

### Credentials Isolation

Each sandbox stores channel tokens and plugin API keys in its own namespace as K8s secrets. Secrets are created by `azureclaw add` and mounted via `envFrom` — they are never shared across namespaces.

```
azureclaw-tenant-a/
  └─ tenant-a-credentials      # All of Tenant A's channel/plugin keys

azureclaw-tenant-b/
  └─ tenant-b-credentials      # All of Tenant B's channel/plugin keys
```

Use `azureclaw credentials update <name>` to rotate credentials for a specific sandbox without affecting others. See [channels-plugins.md](channels-plugins.md#rotating-credentials) for details.

### Channel Isolation

Each sandbox gets its own channel instance — there is no shared bot or message bus:

| Resource | Isolation |
|----------|-----------|
| Telegram bot | Each sandbox uses its own BotFather token; separate polling loop |
| Slack app | Each sandbox uses its own `xoxb-` token; separate WebSocket connection |
| Discord bot | Each sandbox uses its own bot token; separate gateway session |
| WhatsApp | Each sandbox pairs its own QR code session |

This means Tenant A's Telegram bot is completely independent of Tenant B's — different tokens, different chat histories, different message streams.

### Network Isolation

Three layers enforce network boundaries between tenants:

| Layer | Enforcement | Scope |
|-------|------------|-------|
| **iptables** | UID-based egress rules (init container) | Per-container — agent (UID 1000) restricted to localhost + DNS |
| **NetworkPolicy** | Default-deny egress per namespace | Per-namespace — blocks all cross-namespace traffic |
| **Cilium CNI** | Pod-level enforcement on AKS | Cluster-wide — NetworkPolicy backed by eBPF |

Cross-namespace traffic is blocked by default. The only exception is AGT mesh traffic (port 8443) when governance is enabled — this requires an explicit ingress NetworkPolicy created by the controller.

## Content Safety floor

A Kubernetes `ValidatingAdmissionPolicy` (`azureclaw-content-safety-floor`)
enforces a minimum severity threshold on every `InferencePolicy` CR before it
is accepted by the API server.

**How it works:**

The VAP uses a CEL expression to check the `spec.inference.contentSafetyMinimum`
field. Severity is an ordinal: `Safe (0) < Low (1) < Medium (2) < High (3)`.
Lower ordinal = stricter. The default cluster floor is `Medium`, so any
`InferencePolicy` that sets a floor *less strict than `Medium`* (i.e., `Low` or
`Safe`) is rejected at admission.

```yaml
# Helm values to configure the floor
admission:
  contentSafetyFloor:
    enabled: true        # default: true
    minimum: Medium      # default: Medium; valid: Safe | Low | Medium | High
```

**Per-CR developer opt-out (non-production only):**

A sandbox can bypass the floor by labelling the `InferencePolicy` with
`azureclaw.azure.com/dev-only: "true"`. This label is rejected in namespaces
that carry the `azureclaw.azure.com/production: "true"` label — the VAP
enforces this invariant.

**Requirements:** Kubernetes ≥ 1.30 (ValidatingAdmissionPolicy GA).

## A2A public ingress

AzureClaw can expose a sandboxed agent to external A2A (Agent-to-Agent) traffic
via a Kubernetes `LoadBalancer` service fronted by an ingress-layer TLS
termination. This is opt-in at the Helm level.

```yaml
# deploy/helm/azureclaw/values.yaml
a2aGateway:
  enabled: true           # default: false; set to true to provision the gateway
  tlsSecretName: ""       # cert-manager populates this automatically when empty
```

When `a2aGateway.enabled: true` the Helm chart provisions:
- A `LoadBalancer` service exposing port 443 for inbound A2A traffic
- A cert-manager `Certificate` CR for TLS (requires cert-manager ≥ 1.14 in
  the cluster)
- An ingress rule that routes `/.well-known/agent.json` requests to the
  sandbox's A2A card server

**Cross-tenant A2A federation:** an external agent (in another cluster or
tenant) discovers the sandbox via its Agent Card (`/.well-known/agent.json`),
then initiates an authenticated A2A session over the public ingress. The
inference router's A2A handler validates the inbound Agent Card signature and
enforces `ClawPairing` policy before forwarding the request to the sandbox.

**Security note:** enabling public ingress adds a `Microsoft.Network/loadBalancers/write`
action requirement to the deployer role — see [permissions.md](permissions.md)
for details.


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
