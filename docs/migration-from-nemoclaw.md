# Migrating from NemoClaw to AzureClaw

This guide walks you through migrating an existing NemoClaw deployment to AzureClaw.

## Overview

AzureClaw maintains compatibility with NemoClaw's core concepts:

| NemoClaw Concept | AzureClaw Equivalent |
|---|---|
| `nemoclaw onboard` | `azureclaw onboard` |
| `nemoclaw <name> connect` | `azureclaw <name> connect` |
| `nemoclaw <name> status` | `azureclaw <name> status` |
| `nemoclaw <name> logs` | `azureclaw <name> logs` |
| OpenShell sandbox | AKS pod (ClawSandbox CRD) |
| Blueprint (Python artifact) | Blueprint Controller (K8s operator) |
| NVIDIA cloud inference | Azure OpenAI / AI Foundry |
| `openclaw-sandbox.yaml` | `baseline.yaml` (compatible format) |

## Steps

### 1. Install AzureClaw

```bash
npm install -g @azure/azureclaw
```

### 2. Initialize Azure Infrastructure

```bash
az login
azureclaw init --resource-group my-rg --location eastus2
```

### 3. Export NemoClaw Configuration

```bash
# Export your NemoClaw policy
cp ~/.nemoclaw/blueprints/policies/openclaw-sandbox.yaml ./my-policy.yaml
```

### 4. Convert and Apply

```bash
# The migrate command converts NemoClaw policies to AzureClaw format
azureclaw migrate --from-nemoclaw ./my-policy.yaml

# Onboard with your existing configuration
azureclaw onboard --policy ./my-policy.azureclaw.yaml
```

### 5. Verify

```bash
azureclaw my-assistant status
azureclaw my-assistant connect
```

## Policy Compatibility

AzureClaw's baseline policy uses the same YAML schema as NemoClaw's `openclaw-sandbox.yaml` for the network section, with Azure-specific extensions:

**NemoClaw format (supported):**
```yaml
network:
  endpoints:
    - name: github
      host: "github.com"
      port: 443
      binary: "/usr/bin/git"
      methods: ["*"]
```

**AzureClaw extensions:**
```yaml
network:
  endpoints:
    - name: github
      host: "github.com"
      port: 443
      binary: "/usr/bin/git"
      methods: ["*"]
      # AzureClaw additions:
      rateLimit: 100         # requests per minute
      maxBodySize: "10MB"    # request body size limit
      audit: true            # log all requests
```

## Key Differences

1. **Inference:** NemoClaw routes to NVIDIA cloud (Nemotron). AzureClaw routes to Azure OpenAI/AI Foundry with 1800+ models.
2. **Identity:** NemoClaw uses API keys. AzureClaw uses Managed Identity (zero credentials in sandbox).
3. **Isolation:** AzureClaw adds Confidential Containers for hardware-level isolation.
4. **Scale:** AzureClaw runs on AKS (multi-node, multi-region) vs NemoClaw's single-node K3s.
5. **Compliance:** AzureClaw includes azure-osconfig for CIS/STIG baseline enforcement on ACL nodes (TODO).
