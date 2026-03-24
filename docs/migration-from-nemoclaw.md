# Migrating from NemoClaw to AzureClaw

## Concept Mapping

| NemoClaw | AzureClaw |
|---|---|
| `nemoclaw onboard` | `azureclaw credentials` (or just run `azureclaw dev` — prompts inline) |
| `nemoclaw <name> connect` | `azureclaw connect <name>` |
| `nemoclaw <name> status` | `azureclaw status <name>` |
| OpenShell sandbox | AKS pod (ClawSandbox CRD) |
| Blueprint (Python artifact) | Rust K8s controller (kube-rs) |
| NVIDIA cloud inference (Nemotron) | Azure AI Foundry (200+ models) |
| API keys | Managed Identity (zero credentials) |
| K3s (single node) | AKS (multi-node, managed) |
| Landlock + seccomp | seccomp + iptables UID-based + optional Kata VM |

## Steps

```bash
# 1. Install
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli && npm install && npm run build && npm link

# 2. Deploy (creates AKS cluster + sandbox)
az login
azureclaw up --name my-agent --model gpt-4.1

# 3. Connect (same OpenClaw TUI experience)
azureclaw connect my-agent
```

## Key Differences

1. **Inference:** Foundry (200+ models) instead of NVIDIA cloud. The inference router transparently handles auth and content safety.
2. **Identity:** No API keys. Managed Identity (IMDS) authenticates all model calls. Agent container cannot access credentials.
3. **Isolation:** Three levels (standard/enhanced/confidential). Confidential adds a Kata VM per pod.
4. **Network:** iptables UID-based egress + NetworkPolicy + inference-as-network-policy. Agent container can only reach localhost.
5. **Scale:** AKS multi-node with per-namespace tenant isolation.
6. **Safety:** Content Safety + Prompt Shields on every inference call (on by default).

## Network Policy Compatibility

NemoClaw network endpoint format is compatible with AzureClaw's CRD `allowedEndpoints`:

```yaml
# NemoClaw
network:
  endpoints:
    - name: github
      host: "github.com"
      port: 443

# AzureClaw (via CLI)
azureclaw policy allow my-agent github.com
```
