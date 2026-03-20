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

Manually adapt your NemoClaw policy to AzureClaw's ClawSandbox CRD format:

```bash
# Create a ClawSandbox YAML from your exported policy
# See examples/basic-agent/clawsandbox.yaml for the format
azureclaw up --name my-assistant --model gpt-4.1
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
      methods: ["GET"]
```

## Key Differences

1. **Inference:** NemoClaw routes to NVIDIA cloud (Nemotron). AzureClaw routes to Azure OpenAI/AI Foundry with 200+ models.
2. **Identity:** NemoClaw uses API keys. AzureClaw uses Managed Identity (zero credentials in sandbox).
3. **Isolation:** AzureClaw adds Kata VM per-pod isolation for the confidential level.
4. **Scale:** AzureClaw runs on AKS (multi-node) vs NemoClaw's single-node K3s.
5. **Inference safety:** AzureClaw integrates Azure AI Content Safety + Prompt Shields on every inference call.
