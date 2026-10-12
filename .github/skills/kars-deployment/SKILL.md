---
description: "Kars deployment and infrastructure skill — how to deploy, build images, manage AKS, and troubleshoot."
---
<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->


# Kars Deployment & Infrastructure

## Quick Start
```bash
# Login to Azure and ACR
az login
az acr login --name karsacr

# Build all images
make build-all  # or manually:
docker build --platform linux/amd64 -f sandbox-images/openclaw/Dockerfile -t karsacr.azurecr.io/openclaw-sandbox:latest .
docker push karsacr.azurecr.io/openclaw-sandbox:latest

# Deploy to AKS
kars up --resource-group <rg> --isolation confidential
```

## Image Registry (ACR: karsacr)
| Image | Source | Purpose |
|-------|--------|---------|
| `openclaw-sandbox:latest` | `sandbox-images/openclaw/Dockerfile` | Agent container (OpenClaw + Kars mesh plugin) |
| `kars-inference-router:latest` | `inference-router/Dockerfile` | Per-pod proxy (AOAI, relay/registry, native AGT governance) |
| `kars-controller:latest` | `controller/Dockerfile` | K8s controller (CRD reconciler) |
| `agentmesh-relay-agt:latest` | `sandbox-images/agentmesh/Dockerfile`, `COMPONENT=relay` | AL3 distroless upstream AGT WebSocket relay |
| `agentmesh-registry-agt:latest` | `sandbox-images/agentmesh/Dockerfile`, `COMPONENT=registry` | AL3 distroless upstream AGT registry + prekey storage |

## AKS Namespaces
| Namespace | Purpose |
|-----------|---------|
| `kars-system` | Controller + CRDs |
| `agentmesh` | Relay + in-memory Registry |
| `kars-foundry-test` | Parent sandbox (persistent) |
| `kars-<name>` | Spawned sub-agent sandboxes (ephemeral) |

## Controller
- Watches `KarsSandbox` CRDs in `kars-system`
- Creates namespace, deployment, service, networkpolicy for each sandbox
- Defaults: `SANDBOX_IMAGE=openclaw-sandbox:latest`, `INFERENCE_ROUTER_IMAGE=inference-router:latest`
- DO NOT set image env vars manually — use defaults

## AgentMesh Infrastructure
Beta installs manage the mesh in the Core Helm release (`agentMesh.enabled=true`).
Do not bootstrap it with raw manifests alongside Helm-owned resources.
The standalone compatibility deployment is:

```bash
# Standalone only: deploy Microsoft AGT AgentMesh (relay + registry)
kubectl apply -f deploy/agentmesh-agt.yaml

# Verify
kubectl get pods -n agentmesh
# Should show: registry and relay — both Running
```

Build both components from the Kars repo root using the maintained recipe and
validated AGT 5 core/server + native ACS wheels. Do not use upstream's deprecated
`agentmesh-platform` Dockerfile or its removed `server` extra. Server image
qualification does not establish installed identity or encrypted cross-runtime
collaboration acceptance.

## Troubleshooting
- **Pod stuck Pending**: Check node pool capacity (`kubectl describe pod`)
- **ImagePullBackOff**: ACR auth issue (`az acr login --name karsacr`)
- **Controller not reconciling**: Check CRD (`kubectl get karssandbox -A`) and controller logs
- **Relay auth fails**: Check the AGT relay token secret and relay logs
- **Registry 401 on prekey upload**: Check AGT registry auth configuration and pod identity
- **Old image cached on AKS**: Use `imagePullPolicy: Always` for `:latest` tags
