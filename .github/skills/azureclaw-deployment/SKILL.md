---
description: "AzureClaw deployment and infrastructure skill — how to deploy, build images, manage AKS, and troubleshoot."
---

# AzureClaw Deployment & Infrastructure

## Quick Start
```bash
# Login to Azure and ACR
az login
az acr login --name azureclawacr

# Build all images
make build-all  # or manually:
docker build --platform linux/amd64 -f sandbox-images/openclaw/Dockerfile -t azureclawacr.azurecr.io/openclaw-sandbox:latest .
docker push azureclawacr.azurecr.io/openclaw-sandbox:latest

# Deploy to AKS
azureclaw up --resource-group <rg> --isolation confidential
```

## Image Registry (ACR: azureclawacr)
| Image | Source | Purpose |
|-------|--------|---------|
| `openclaw-sandbox:latest` | `sandbox-images/openclaw/Dockerfile` | Agent container (OpenClaw + AzureClaw mesh plugin) |
| `azureclaw-inference-router:latest` | `inference-router/Dockerfile` | Per-pod proxy (AOAI, relay/registry, native AGT governance) |
| `azureclaw-controller:latest` | `controller/Dockerfile` | K8s controller (CRD reconciler) |
| `agentmesh-relay-agt:latest` | Microsoft AGT AgentMesh image | WebSocket relay server |
| `agentmesh-registry-agt:latest` | Microsoft AGT AgentMesh image | Agent discovery + prekey storage |

## AKS Namespaces
| Namespace | Purpose |
|-----------|---------|
| `azureclaw-system` | Controller + CRDs |
| `agentmesh` | Relay + Registry + PostgreSQL |
| `azureclaw-foundry-test` | Parent sandbox (persistent) |
| `azureclaw-<name>` | Spawned sub-agent sandboxes (ephemeral) |

## Controller
- Watches `ClawSandbox` CRDs in `azureclaw-system`
- Creates namespace, deployment, service, networkpolicy for each sandbox
- Defaults: `SANDBOX_IMAGE=openclaw-sandbox:latest`, `INFERENCE_ROUTER_IMAGE=inference-router:latest`
- DO NOT set image env vars manually — use defaults

## AgentMesh Infrastructure
```bash
# Deploy Microsoft AGT AgentMesh (relay + registry + postgres)
kubectl apply -f deploy/agentmesh-agt.yaml

# Verify
kubectl get pods -n agentmesh
# Should show: postgres, registry, relay — all Running
```

## Troubleshooting
- **Pod stuck Pending**: Check node pool capacity (`kubectl describe pod`)
- **ImagePullBackOff**: ACR auth issue (`az acr login --name azureclawacr`)
- **Controller not reconciling**: Check CRD (`kubectl get clawsandbox -A`) and controller logs
- **Relay auth fails**: Check the AGT relay token secret and relay logs
- **Registry 401 on prekey upload**: Check AGT registry auth configuration and pod identity
- **Old image cached on AKS**: Use `imagePullPolicy: Always` for `:latest` tags
