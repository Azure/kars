# Blueprint 02 — Enterprise self-hosted cluster

> "I'm a platform team inside one organisation. I want to give my engineers and product teams a hardened, governed AI agent runtime on AKS that I own end-to-end — same Entra tenant, same network island, same audit destination, no third-party SaaS in the data path."

## Persona & intent

- **You are:** the platform / infra / SRE team inside one company. You own an Azure subscription, an Entra tenant, and a security-approved Azure AI Foundry project.
- **You want:** to run AzureClaw as a single-tenant cluster for *your own* employees and *your own* services. Anyone consuming agents is inside the same Entra tenant or paired in via your operator-managed pairing tokens.
- **You do not want:** any agent traffic to leave your VNet. Any provider you can't audit in the data path. Any plaintext in the relay.

## Topology

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#f1f5f9','primaryBorderColor':'#475569','primaryTextColor':'#0f172a','lineColor':'#475569','clusterBkg':'#f8fafc','clusterBorder':'#94a3b8','fontFamily':'-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif'}}}%%
flowchart TB
  subgraph Corp["🏢 Your organisation (single Entra tenant)"]
    direction TB

    subgraph Ops["Platform team"]
      OpsLaptop["operator workstation<br/>azureclaw CLI + kubectl"]
    end

    subgraph Azure["☁️ Your Azure subscription"]
      direction TB
      AKS[("☸️ AKS<br/>Cilium + Workload Identity")]
      ACR[("📦 ACR (private)")]
      AGW[("🛡 App Gateway + WAF<br/>private endpoint")]
      FND[("🧠 Foundry project<br/>Content Safety + memory store")]
      KV[("🔐 Key Vault")]
      LAW[("📊 Log Analytics + App Insights")]

      subgraph AKSContent[" "]
        direction TB
        CTRL["azureclaw-controller"]
        REG["mesh registry"]
        RLY["mesh relay"]
        PG[("Postgres")]

        subgraph SBX1["📦 ClawSandbox 'research-bot'"]
          OC1["openclaw (UID 1000)"]
          IR1["router (UID 1001)"]
        end
        subgraph SBX2["📦 ClawSandbox 'ops-bot'"]
          OC2["openclaw"]
          IR2["router"]
        end
      end
      AKS --- AKSContent
    end

    subgraph Users["👥 Your employees"]
      Eng["engineer<br/>(NemoClaw / OpenClaw on laptop)"]
      Slack["Slack / Telegram channel"]
    end
  end

  OpsLaptop -->|"azureclaw up<br/>azureclaw add"| AKS
  IR1 -->|"Workload Identity<br/>OIDC"| FND
  IR2 -->|"Workload Identity"| FND
  IR1 -.->|"audit chain"| LAW
  IR2 -.->|"audit chain"| LAW
  AGW -->|"private endpoint"| REG
  AGW -->|"private endpoint"| RLY
  REG --- PG
  RLY --- PG
  KV -.->|"secrets via CSI"| AKS

  OpsLaptop -->|"azureclaw mesh promote<br/>--allow-ip <corp-cidr><br/>azureclaw pair generate"| AGW
  Eng -->|"one-time token<br/>in NemoClaw chat"| AGW

  Slack -.->|"webhook"| OC1
```

## Trust boundary

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#f1f5f9','primaryBorderColor':'#475569','primaryTextColor':'#0f172a','lineColor':'#475569','clusterBkg':'#f8fafc','clusterBorder':'#94a3b8','fontFamily':'-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif'}}}%%
flowchart LR
  subgraph TB1["🏢 Corp Entra tenant + VNet"]
    direction TB
    Eng["employee laptop"]
    AKS["AzureClaw AKS"]
    FND["Foundry"]
    LAW["Log Analytics"]
  end

  Eng -->|"E2E (Signal Protocol)<br/>via private App Gateway"| AKS
  AKS -->|"Workload Identity"| FND
  AKS -->|"audit"| LAW

  classDef boundary stroke:#1e40af,stroke-width:3px,fill:#dbeafe;
  class TB1 boundary;
```

- **Single trust domain.** Everything inside the Entra tenant.
- **No cleartext at rest** — pairing token hashes only, audit chain hash-chained, mesh sessions Double-Ratchet keyed.
- **No cleartext in flight** — App Gateway private endpoint terminates corp TLS; relay traffic remains Signal-protocol-encrypted end-to-end *inside* the TLS tunnel.

## Primary flow — onboarding a new employee laptop

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#f1f5f9','primaryBorderColor':'#475569','primaryTextColor':'#0f172a','lineColor':'#475569','clusterBkg':'#f8fafc','clusterBorder':'#94a3b8','fontFamily':'-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif'}}}%%
sequenceDiagram
    autonumber
    participant Eng as 👤 Engineer
    participant Ops as Platform team
    participant CLI as azureclaw CLI
    participant Ctrl as controller
    participant Reg as registry
    participant Rly as relay
    participant Plg as azureclaw-mesh plugin (in NemoClaw)

    Ops->>CLI: azureclaw mesh promote --allow-ip 10.0.0.0/8
    CLI->>Ctrl: provision App Gateway endpoint, IP allowlist
    Ops->>CLI: azureclaw pair generate --name alice-laptop --slots 3
    CLI-->>Ops: azc_pair_v1_… token
    Ops->>Eng: secret-share / Teams DM / SMS<br/>(secure channel)
    Eng->>Plg: paste token in NemoClaw chat<br/>(or set AZURECLAW_PAIRING_TOKEN env)
    Plg->>Reg: claim slot + register Ed25519 identity
    Plg->>Rly: KNOCK + X3DH prekeys
    Rly-->>Plg: session established
    Note over Eng,Plg: Pairing complete.<br/>Token is single-use, auto-zeroized.

    Eng->>Plg: "offload analyze_repo to AzureClaw"
    Plg->>Rly: encrypted offload request
    Rly->>Ctrl: deliver to controller AMID
    Ctrl->>Ctrl: spawn ClawSandbox offload-N (token budget enforced)
    Ctrl-->>Plg: result via E2E reply
```

## What you provision

```bash
# One-time per cluster
azureclaw up                                      # AKS + ACR + Foundry + Key Vault + initial sandboxes
azureclaw operator                                # live TUI for the cluster

# Per agent
azureclaw add research-bot --model gpt-4.1 --governance --learn-egress
azureclaw add ops-bot --model gpt-5-mini --governance
azureclaw credentials update research-bot --telegram-token "<bot-token>"

# Onboard a NemoClaw / OpenClaw user (no AzureClaw CLI on their laptop)
azureclaw mesh promote --allow-ip 10.0.0.0/8      # one-time, exposes registry+relay over private App Gateway
azureclaw pair generate --name alice-laptop --slots 3 --capabilities offload,handoff

# Day-2 ops
azureclaw policy allow research-bot api.example.com
azureclaw model set research-bot gpt-5-mini
azureclaw egress research-bot --learned
azureclaw trace research-bot --network
```

## What's unique to this blueprint

- **Single tenant, single audit destination.** Everything an employee or a CI job does flows into your Log Analytics + audit chain. No third party.
- **Workload Identity instead of API keys.** The router binds to a federated K8s ServiceAccount → Entra workload identity. Foundry sees the request as your tenant.
- **Pairing replaces VPN-for-agents.** Employees don't need a VPN tunnel to AzureClaw — they get a one-time token that scopes them to one slot of one capability set with one budget cap. Lost laptop = revoke one Pairing CR.
- **You can scale Confidential Containers in.** AKS supports kata + AMD SEV-SNP node pools today. Set `ClawSandbox.spec.isolation: confidential` per-agent for sensitive workloads; sub-agents inherit and cannot downgrade.

## What this blueprint is NOT

- Not a multi-tenant SaaS. If you serve external customers, see Blueprint 03.
- Not a federation pattern. If you collaborate with another org's AzureClaw, see Blueprint 04.
- Not air-gapped. If your network can't reach Foundry, see Blueprint 05.

## References

- `cli/src/commands/up.ts` (Bicep + Helm provisioning)
- `controller/src/reconciler/mod.rs` (sandbox composition)
- `controller/src/pairing.rs` + `cli/src/commands/pair.ts` (token issuance)
- `inference-router/src/auth.rs` (Workload Identity OIDC exchange)
- `deploy/helm/azureclaw/values.yaml` (Helm contract)
- ADR-0001 — A2A ingress front-edge (`docs/adr/0001-a2a-ingress-front-edge.md`)
