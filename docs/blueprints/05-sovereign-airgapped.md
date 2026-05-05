# Blueprint 05 — Sovereign / air-gapped

> "We run regulated, classified, sovereign-cloud, or fully air-gapped workloads. There is no public internet. There is no commercial Foundry endpoint. There is no Microsoft-hosted MCP catalogue. We still want AzureClaw's isolation + governance + audit guarantees, on locally-hosted models, with everything reproducible from a signed bundle."

> **Status: 🚧 Patterns documented; reproducible-bundle tooling on roadmap.** Today this blueprint is achievable by hand using the standard CRDs + a private model endpoint; the goal is a one-command `azureclaw bundle` that emits a signed, reproducible offline kit.

## Persona & intent

- **You are:** a defence, intelligence, regulator, financial-services, or sovereign-cloud operator. Or an enterprise that has chosen to self-host LLMs.
- **You want:** AzureClaw's threat model, but with the model running on private hardware (e.g. Foundry-Edge, vLLM, llama.cpp, ONNX Runtime, an on-prem Triton) and zero traffic crossing the network island.
- **You do not want:** any default outbound destination — every domain in the blocklist, the Foundry SDK, Application Insights, and the audit sink — to be assumed reachable.
- **Runtime:** choose `spec.runtime.kind: OpenClaw` (default) for zero agent-code changes, or `BYO` for a custom container that satisfies the [BYO contract](../runtimes.md#the-contract-your-image-must-satisfy). Python teams using the OpenAI Agents SDK, Microsoft Agent Framework (Python), LangGraph (Python or TypeScript), Anthropic Claude SDK, or Pydantic-AI can use the matching first-class adapter — same isolation and governance apply. `SemanticKernel` is reserved in the CRD enum but the adapter image is not yet built (emits `AdapterMissing`).

## Topology

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#f1f5f9','primaryBorderColor':'#475569','primaryTextColor':'#0f172a','lineColor':'#475569','clusterBkg':'#f8fafc','clusterBorder':'#94a3b8','fontFamily':'-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif'}}}%%
flowchart TB
  subgraph AirGap["🔒 Air-gapped network island"]
    direction TB

    subgraph Bundle["📦 signed offline bundle"]
      Imgs["controller, router, sandbox<br/>(cosign-signed images)"]
      Helm["helm charts + values"]
      Mdl["model weights + tokenizer"]
      Pol["policy profiles + blocklists"]
      Doc["bundle manifest + SBOM"]
    end

    subgraph AKS["☸️ Local AKS / Azure Stack HCI / k3s"]
      direction TB
      Reg2[("private registry<br/>(no public ACR)")]
      Ctrl["controller"]
      RlyL["relay (cluster-local only)"]
      RegL["registry (cluster-local only)"]
      Mdlsvc["model server<br/>(Foundry-Edge / vLLM / Triton)"]

      subgraph SBX["📦 ClawSandbox 'analyst'"]
        OC["openclaw"]
        IR["router<br/>(--local-model https://model-svc)"]
      end
    end

    subgraph SIEM["📊 local SIEM"]
      Splunk["Splunk / Sentinel On-Prem"]
    end

    Bundle -->|"transferred via<br/>signed media"| Reg2
    Reg2 --> AKS

    IR -->|"only outbound:<br/>cluster-local model"| Mdlsvc
    IR -.->|"audit chain"| Splunk
  end

  subgraph Outside["🌐 Outside (DOES NOT HAPPEN)"]
    Public["public Foundry, Microsoft.com,<br/>any blocklist update,<br/>any telemetry"]
  end

  AirGap -. "❌ no path"  .- Outside

  classDef gap fill:#0c0c1f,stroke:#fbbf24,color:#fff;
  classDef out fill:#fee2e2,stroke:#dc2626,color:#7f1d1d;
  class AirGap,AKS,Bundle,SIEM gap;
  class Outside,Public out;
```

## Trust boundary

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#f1f5f9','primaryBorderColor':'#475569','primaryTextColor':'#0f172a','lineColor':'#475569','clusterBkg':'#f8fafc','clusterBorder':'#94a3b8','fontFamily':'-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif'}}}%%
flowchart TB
  subgraph Inside["🔒 Air-gapped trust domain"]
    direction LR
    Agent["agent (UID 1000)"]
    Router["router (UID 1001)"]
    Model["model server"]
    Audit["local SIEM"]
  end

  Agent -->|"localhost only"| Router
  Router -->|"cluster-local DNS"| Model
  Router -.->|"audit chain"| Audit

  classDef gap fill:#0c0c1f,stroke:#fbbf24,color:#fff;
  class Inside gap;
```

The trust boundary is the **network island**. Nothing inside it talks to anything outside it; nothing outside it talks to anything inside it. The router's allow-list is configured to a single internal model service DNS name; the blocklist is irrelevant because the egress NetworkPolicy denies *everything else* by default.

## Primary flow — bundle build, transfer, install

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#f1f5f9','primaryBorderColor':'#475569','primaryTextColor':'#0f172a','lineColor':'#475569','clusterBkg':'#f8fafc','clusterBorder':'#94a3b8','fontFamily':'-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif'}}}%%
sequenceDiagram
    autonumber
    participant Build as build host (online)
    participant Sign as cosign / sigstore
    participant Media as physical media / one-way diode
    participant Air as air-gapped admin
    participant Reg as private registry
    participant Cls as local cluster

    Build->>Build: make bundle<br/>(images + helm + weights + policy)
    Build->>Sign: cosign sign + SBOM
    Sign-->>Build: signature attestations
    Build->>Media: write bundle.tar.gz + sigs
    Media->>Air: physical transfer
    Air->>Air: cosign verify (offline, public key only)
    Air->>Reg: docker load + push
    Air->>Cls: helm install azureclaw \<br/>--values offline-values.yaml \<br/>--set router.model.localUrl=https://model-svc
    Cls->>Cls: ClawSandbox spawn,<br/>NetworkPolicy enforces<br/>internal-only egress
```

## What you provision

### Runtime and CRD model in air-gap

All eight CRDs work offline. The runtime adapter and model connection are configured via Helm values, not by choosing a different CRD schema:

| CRD | Air-gap role |
|---|---|
| `InferencePolicy` | Token budget (daily/monthly) + content safety against the local model. Referenced by `ClawSandbox.spec.inferenceRef.name` (omitting it → `Degraded/InferencePolicyNotFound`). |
| `ToolPolicy` | Per-tool rate limits and spend caps for local tool servers (e.g., code search over cluster-local MCP). |
| `McpServer` | Declare cluster-local private MCP servers. No OAuth unless you run an on-prem IdP. |
| `ClawMemory` | Bind to an on-prem Foundry-Edge or compatible memory-store endpoint. |
| `ClawEval` | Schedule regression evals against your local model server. |

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: analyst-policy
  namespace: azureclaw-analyst
spec:
  tokenBudget:
    dailyTokens: 500000
    monthlyTokens: 10000000
  contentSafety:
    requirePromptShields: false   # local model; no Azure Content Safety endpoint
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: analyst
  namespace: azureclaw-analyst
spec:
  runtime:
    kind: OpenClaw               # or BYO if you supply your own container
  inferenceRef:
    name: analyst-policy         # InferencePolicy ref
  networkPolicy:
    allowlistRef:                # signed OCI artifact bundled offline (see below)
      registry: private-registry.local
      repository: azureclaw-policy/analyst-egress
      digest: sha256:…
      artifactType: application/vnd.azureclaw.egress-allowlist.v1+yaml
```

### Signed OCI egress allowlist — offline / bundle signing

In an air-gapped deployment the allowlist artifact is built and signed on the online build host, then transferred in the same bundle as the images:

```bash
# === Online build host ===

# 1. Sign the egress allowlist (kms mode for sovereign key custody,
#    or keyless on an OIDC-capable CI runner):
azureclaw egress sign \
  --allowlist analyst-egress.yaml \
  --push private-registry.local/azureclaw-policy/analyst-egress \
  --mode kms \
  --kms-key https://myvault.vault.azure.net/keys/airgap-signer/v1

# 2. Save the artifact + signature to the bundle tarball:
cosign save private-registry.local/azureclaw-policy/analyst-egress \
  --dir bundle/policy/analyst-egress/

# 3. Install the SignerPolicy ConfigMap in the bundle values
#    (applied by the offline helm install):
cat bundle/values/offline-values.yaml
# …
# signerPolicy:
#   fulcioIssuers: []          # not used in kms mode
#   sanPatterns: []
#   kmsKeyId: "https://myvault.vault.azure.net/keys/airgap-signer/v1"
```

```bash
# === Air-gapped admin host ===

# 4. Load the policy artifact into the private registry:
cosign load \
  --dir bundle/policy/analyst-egress/ \
  private-registry.local/azureclaw-policy/analyst-egress

# 5. Helm install applies the SignerPolicy ConfigMap automatically.
#    The controller verifies the signature on first reconcile.
#    AllowlistVerified=True → NetworkPolicy applied.
#    AllowlistVerified=False/SignerPolicyMissing → sandbox stays safe (fail-closed).
```

**Signing modes in air-gap:**
- `kms` — use Azure Key Vault (on-prem or Azure Stack) for sovereign key custody. The signing key never leaves the HSM.
- `keyless` — works if the build host has an OIDC issuer (GitHub Actions, Azure Pipelines) and you transfer the Fulcio certificate bundle in the offline kit. Set `fulcioIssuers` + `sanPatterns` in the ConfigMap.

### CLI commands

```bash
# On the (online) build host:
make bundle                                       # 🚧 roadmap; today: assemble manually
cosign sign-blob ./bundle.tar.gz \
  --key cosign.key \
  --output-file bundle.sig

# Physical / diode transfer.

# On the air-gapped admin host:
cosign verify-blob ./bundle.tar.gz \
  --key cosign.pub \
  --signature ./bundle.sig
docker load < bundle.tar.gz
docker push private-registry.local/azureclaw/{controller,router,sandbox}:latest

helm install azureclaw deploy/helm/azureclaw \
  --values offline-values.yaml \
  --set image.repository=private-registry.local/azureclaw \
  --set router.model.provider=local-openai-compat \
  --set router.model.endpoint=https://model-svc.ml.svc.cluster.local \
  --set audit.sink=splunk-hec://siem.local

azureclaw add analyst --model llama-3.1-70b --governance \
  --egress-policy deny-all-except=model-svc.ml.svc.cluster.local
```

## What's unique to this blueprint

- **Local-model adapter.** The router has an OpenAI-compatible local-model adapter (`router.model.provider=local-openai-compat`) so any model server speaking that wire format slots in. No code change to the agent; the same `gpt-4.1`-style interface is preserved.
- **BYO runtime or existing agent code.** Use `spec.runtime.kind: BYO` for a custom container declaring the `org.azureclaw.runtime.contract` OCI label. Teams with existing Python OpenAI Agents or MAF code can use the first-class adapters (`OpenAIAgents`, `MicrosoftAgentFramework`) without changes to the governance or audit chain.
- **Egress NetworkPolicy is the primary control,** not the 51k-domain blocklist. The blocklist is irrelevant when default-deny is enforced and only one internal DNS name is allowed.
- **Signed OCI egress allowlist — air-gap / offline / KMS path.** Build and sign the allowlist artifact on the online build host; include it in the bundle; load it into the private registry on the air-gapped side. Use `--mode kms` with an on-prem or Azure Stack Key Vault for sovereign key custody. The controller verifies on every reconcile and fails closed if the signature is absent or invalid.
- **Audit chain stays local.** The default `AuditSink` writes to a configurable destination (App Insights, Log Analytics, Splunk HEC, file). For sovereign deployments, a Splunk HEC or local file backend is the typical choice; the hash chain is preserved either way.
- **No telemetry leaks.** All Microsoft-hosted telemetry (App Insights, Microsoft Defender for Cloud) is off-by-default and replaced by your local SIEM.
- **Cosign-signed bundle.** The reproducible bundle is the only authenticated trust root; the air-gapped side has only a public key.

## What this blueprint is NOT

- Not "regular AzureClaw with no internet." Foundry, the blocklist refresh, and several telemetry paths assume reachability and must be deliberately disabled or replaced.
- Not a substitute for cross-domain solutions (CDS) — this blueprint covers the runtime; data ingestion / sanitisation across the boundary is your CDS team's problem.
- Not a fast onramp. Building a verified bundle, transferring it, and standing up a local cluster is multi-step. The reward is reproducibility.

## Bundle contents (current target)

```
bundle.tar.gz
├── manifest.yaml                    # bundle version + signed SBOM
├── images/
│   ├── controller-vX.tar
│   ├── router-vX.tar
│   └── sandbox-vX.tar
├── helm/
│   └── azureclaw-vX.tgz
├── policy-profiles/
│   ├── seccomp/azureclaw-strict.json
│   └── agt/{policy,trust,audit,rate-limit}.yaml
├── blocklists/
│   └── domains.txt                  # snapshot, since auto-refresh is off
├── values/
│   └── offline-values.yaml
└── attestations/
    ├── sbom.cdx.json                # CycloneDX SBOM
    └── cosign.sig
```

## References

- `inference-router/src/foundry.rs` (provider switch incl. `local-openai-compat` mode)
- `deploy/helm/azureclaw/values.yaml` (`audit.sink`, `router.model.provider`, `signerPolicy.*`)
- `cli/profiles/` (offline-portable policy bundle)
- `controller/src/policy_fetcher.rs` (allowlist fetch + offline KMS verify)
- `Makefile` `bundle` target (🚧 to be added)
- `docs/api/crd-reference.md` (all 8 CRDs; `spec.runtime.kind` enum; `spec.networkPolicy.allowlistRef.*`)
- `docs/internal/policy-canonical-format.md` (signed OCI egress allowlist format + signing modes)
- `docs/security.md` § "Air-gapped operating mode"
