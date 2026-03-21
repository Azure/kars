---
name: azureclaw-spawn
description: Spawn new AzureClaw sandboxed agents or sub-agents on the AKS cluster. Creates isolated sandboxes with their own inference router, security controls, and optional AGT governance.
metadata: {"openclaw": {"requires": {"env": ["SANDBOX_NAME"]}, "primaryEnv": "SANDBOX_NAME"}}
---

# AzureClaw Spawn — Create Sub-Agents

You can spawn new sandboxed agents on the AKS cluster. Each spawned agent gets its own:
- Isolated namespace with NetworkPolicy
- Inference router sidecar (IMDS auth, Content Safety, token budgets)
- Domain blocklist (auto-refreshing threat intelligence)
- Optional AGT governance (trust-gated mesh communication back to you)

## When to use this

- **Delegation**: spawn a specialist agent for a specific task (e.g., security audit, data analysis)
- **Parallel work**: run multiple agents simultaneously on different tasks
- **Isolation**: give untrusted code its own sandbox with tighter controls

## How to spawn

Use `kubectl` to create a ClawSandbox CRD. The controller will reconcile it into a running sandbox.

### Basic spawn (same model)

```bash
kubectl apply -f - <<EOF
{
  "apiVersion": "azureclaw.azure.com/v1alpha1",
  "kind": "ClawSandbox",
  "metadata": {"name": "sub-analyst", "namespace": "azureclaw-system"},
  "spec": {
    "openclaw": {"version": "2026.3.13", "config": {"agent": {"model": "azure/gpt-4.1"}}},
    "sandbox": {"isolation": "enhanced"},
    "inference": {"model": "gpt-4.1", "contentSafety": true, "promptShields": true},
    "networkPolicy": {"defaultDeny": true}
  }
}
EOF
```

### Spawn with AGT governance (enables mesh communication)

```bash
kubectl apply -f - <<EOF
{
  "apiVersion": "azureclaw.azure.com/v1alpha1",
  "kind": "ClawSandbox",
  "metadata": {"name": "sub-analyst", "namespace": "azureclaw-system"},
  "spec": {
    "openclaw": {"version": "2026.3.13", "config": {"agent": {"model": "azure/gpt-4.1"}}},
    "sandbox": {"isolation": "enhanced"},
    "inference": {"model": "gpt-4.1", "contentSafety": true, "promptShields": true},
    "networkPolicy": {"defaultDeny": true},
    "governance": {"enabled": true, "toolPolicy": "default", "trustThreshold": 500}
  }
}
EOF
```

### Spawn with learn mode (discover needed domains)

```bash
kubectl apply -f - <<EOF
{
  "apiVersion": "azureclaw.azure.com/v1alpha1",
  "kind": "ClawSandbox",
  "metadata": {"name": "sub-analyst", "namespace": "azureclaw-system"},
  "spec": {
    "openclaw": {"version": "2026.3.13", "config": {"agent": {"model": "azure/gpt-4.1"}}},
    "sandbox": {"isolation": "enhanced"},
    "inference": {"model": "gpt-4.1", "contentSafety": true, "promptShields": true},
    "networkPolicy": {"defaultDeny": true, "learnEgress": true}
  }
}
EOF
```

## Check spawn status

```bash
kubectl get clawsandbox -n azureclaw-system
kubectl get pods -n azureclaw-sub-analyst
```

## Communicate with spawned agent (AGT mesh)

If governance is enabled on both you and the sub-agent, use the AGT mesh:

```bash
# Send task to sub-agent
curl -s -X POST http://localhost:8443/agt/mesh/send \
  -H 'Content-Type: application/json' \
  -d '{"to_agent": "sub-analyst", "content": "Analyze the security posture of our k8s cluster", "type": "task_request"}'

# Check for response
curl -s http://localhost:8443/agt/mesh/inbox
```

## Tear down spawned agent

```bash
kubectl delete clawsandbox sub-analyst -n azureclaw-system
```

## Important notes

- The sub-agent runs in its **own isolated namespace** — it cannot see your files or memory
- AGT mesh is the only way to communicate between agents (trust-gated, audited)
- Each sub-agent has its own token budget, blocklist, and Content Safety
- The sandbox name must be unique across the cluster
- Memory Store scopes are per-agent — use the agent's `SANDBOX_NAME` env var as scope
