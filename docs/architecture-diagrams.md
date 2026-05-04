# Architecture diagrams

Every diagram on this page is rendered from Mermaid in the source markdown. The rendered site (mdBook) shows them as SVG; on GitHub they render natively. If you are reading the source, paste any code block into [mermaid.live](https://mermaid.live) for a rendered preview.

For the prose explanation, see **[Architecture](architecture.md)**.

---

## 1. Sandbox pod — dev mode

One container, no network isolation, runs on Docker Desktop.

```mermaid
flowchart TB
  subgraph Laptop["💻 Your laptop (Docker Desktop)"]
    subgraph Sandbox["one container — same image"]
      Agent["Agent runtime<br/>(OpenClaw / OpenAI / MAF / LangGraph / …)"]
      Router["Inference router (Rust)<br/>127.0.0.1:8443"]
      Agent -->|localhost| Router
    end
    Secret["mounted secret<br/>(your Foundry key)"]
    Secret -.-> Router
  end

  Foundry["Azure AI Foundry<br/>model + content safety"]
  Router -->|"HTTPS<br/>(your key)"| Foundry

  classDef laptop fill:#fff5e6,stroke:#cc8800
  class Laptop laptop
```

**What is real:** the router code path, every policy decision, the audit format, the governance profile. **What is not real:** the network isolation — there is no separate process to break out *to*. Treat dev mode as a development surface, not a security surface.

---

## 2. Sandbox pod — prod mode

Multi-container Kubernetes pod, hard egress isolation, Workload Identity.

```mermaid
flowchart TB
  subgraph Cluster["AKS cluster — namespace: azureclaw-&lt;name&gt;"]
    subgraph Pod["ClawSandbox pod"]
      Init["init: egress-guard<br/>(iptables — only UID 1001 egresses)"]
      Agent["agent — UID 1000<br/>no direct egress<br/>(your runtime)"]
      Router["inference-router — UID 1001<br/>127.0.0.1:8443 + 8444"]
      Init -.->|runs first| Agent
      Init -.->|runs first| Router
      Agent -->|localhost only| Router
    end
    NP["NetworkPolicy<br/>egress: DNS · Foundry · relay · A2A gw"]
    NP -.- Pod
    SA["projected SA token"]
    SA -.-> Router
  end

  WI["Workload Identity<br/>(AAD federated token)"]
  Foundry["Azure AI Foundry"]
  Mesh["AgentMesh relay+registry<br/>(another namespace)"]
  A2A["A2A gateway<br/>(public ingress)"]

  Router -->|exchange SA→AAD| WI
  WI -.->|no keys on disk| Router
  Router -->|HTTPS, AAD token| Foundry
  Router -->|WS, encrypted| Mesh
  Router -->|HTTPS| A2A

  classDef cluster fill:#e6f0ff,stroke:#0078d4
  classDef pod fill:#f0fff0,stroke:#2a9d2a
  class Cluster cluster
  class Pod pod
```

**Three containers, one rule:** the agent container has no path to the network. Anything labelled `Foundry` / `Mesh` / `A2A` above leaves through the router. iptables (egress-guard) and NetworkPolicy enforce this in two independent layers.

---

## 3. The data path of one model call

What happens when the agent calls the model. Every other external call (web fetch, MCP tool, sub-agent spawn, A2A peer message) follows the same shape with a different policy module.

```mermaid
sequenceDiagram
  autonumber
  participant Agent as Agent (UID 1000)
  participant Router as Router (UID 1001)
  participant Gov as Governance (AGT + InferencePolicy)
  participant CS as Content Safety (Foundry)
  participant WI as Workload Identity
  participant Foundry as Foundry model

  Agent->>Router: POST /v1/chat (prompt)
  Router->>CS: prompt safety scan
  CS-->>Router: ok
  Router->>Gov: allow? token budget? rate?
  Gov-->>Router: allow + decision audit
  Router->>WI: exchange SA token → AAD
  WI-->>Router: bearer token
  Router->>Foundry: POST /openai/… + bearer
  Foundry-->>Router: completion
  Router->>CS: response safety scan
  CS-->>Router: ok
  Router-->>Agent: completion
  Note over Router: audit record:<br/>hash-chained, signed
```

The agent has no direct path to **Foundry**, **WI**, **CS**, or the audit chain. The router brokers all of them.

---

## 4. The mesh — encrypted inter-agent messaging

Two AzureClaw agents in (possibly) different clusters that need to talk.

```mermaid
sequenceDiagram
  autonumber
  participant A as Alice agent
  participant Ar as Alice router
  participant Reg as AgentMesh registry
  participant Rel as AgentMesh relay
  participant Br as Bob router
  participant B as Bob agent

  Note over A,B: Setup (once per agent)
  A->>Ar: register identity + prekeys
  Ar->>Reg: PUT /agents/alice (Ed25519 sig)
  B->>Br: register identity + prekeys
  Br->>Reg: PUT /agents/bob

  Note over A,B: Session
  A->>Ar: send msg → bob
  Ar->>Reg: GET /agents/bob/prekeys
  Reg-->>Ar: signed prekeys
  Ar->>Ar: X3DH → shared secret
  Ar->>Rel: KNOCK + ciphertext (E2E)
  Rel->>Br: forward ciphertext (relay sees nothing)
  Br->>Br: trust score check (AGT_TRUST_THRESHOLD)
  alt accept
    Br->>B: deliver plaintext
    B-->>Br: reply
    Br->>Rel: ratcheted ciphertext
    Rel->>Ar: forward
    Ar-->>A: plaintext reply
  else deny
    Br-->>Rel: KNOCK denied (audit)
  end
```

**Forward secrecy:** every message after the first uses a fresh key derived by the Double Ratchet. **Authenticated:** every message carries a libsodium MAC. **Relay-blind:** the relay can route, count, and rate-limit, but cannot read. **Trust-gated:** AGT decides per-peer whether the KNOCK is accepted.

---

## 5. A2A gateway — public-ingress peer traffic

For cross-organisation peers that are not in your AgentMesh.

```mermaid
flowchart LR
  subgraph Internet
    Peer["external peer agent<br/>(other org)"]
  end

  subgraph Cluster["AKS cluster"]
    Ing["Public ingress<br/>(App Gateway / k8s ingress)"]
    GW["A2A gateway (Rust)<br/>verifies signed AgentCard<br/>routes to A2AAgent CRD<br/>audit · rate limit · CS"]
    subgraph NS["azureclaw-prod-agent"]
      Pod["ClawSandbox pod<br/>(agent + router)"]
    end
  end

  Peer -->|signed AgentCard + payload| Ing
  Ing --> GW
  GW -->|in-cluster only| Pod
  GW -.->|reject<br/>untrusted card| Peer
```

The A2A gateway is the only inbound public surface. Every request must carry a signed `AgentCard` that the gateway verifies against a configured trust anchor; every request gets the same content-safety / rate-limit treatment as outbound traffic.

---

## 6. Control plane — what the controller does

```mermaid
flowchart LR
  User["operator / CLI / GitOps"]
  CRD[("8 CRDs<br/>ClawSandbox · A2AAgent · McpServer<br/>ToolPolicy · InferencePolicy<br/>ClawMemory · ClawEval · TrustGraph")]
  Ctrl["azureclaw-controller<br/>(kube-rs)"]

  User -->|kubectl apply / azureclaw cli| CRD
  CRD --> Ctrl
  Ctrl --> NS["Namespace + RBAC"]
  Ctrl --> Dep["Deployment / Pod<br/>(agent + router + egress-guard)"]
  Ctrl --> Svc["Service"]
  Ctrl --> NP["NetworkPolicy"]
  Ctrl --> CM["ConfigMap<br/>(governance profile)"]
  Ctrl --> FedCred["Federated credentials<br/>(Workload Identity)"]
  Ctrl --> Status["CRD status<br/>conditions"]

  Status --> User
```

The controller is a vanilla kube-rs reconciler. It owns the eight CRDs, watches them, and produces the boring Kubernetes objects that make a sandbox real. The CRD `status.conditions` chain is the operator-facing source of truth; every condition is documented in **[`docs/api/conditions.md`](api/conditions.md)**.

---

## 7. CRD relationships

How the eight CRDs reference each other.

```mermaid
flowchart TB
  CS["ClawSandbox<br/>(the agent)"]
  TP["ToolPolicy<br/>(allow / deny / approval)"]
  IP["InferencePolicy<br/>(model · tokens · region)"]
  CM["ClawMemory<br/>(memory store binding)"]
  Mcp["McpServer<br/>(allowed MCP backends)"]
  A2A["A2AAgent<br/>(public-ingress endpoint)"]
  TG["TrustGraph<br/>(mesh trust topology)"]
  CE["ClawEval<br/>(reproducible eval run)"]

  CS -->|policyRef| TP
  CS -->|inferenceRef| IP
  CS -->|memoryRef| CM
  CS -->|mcpRefs| Mcp
  CS -->|trustRef| TG
  A2A -->|sandboxRef| CS
  CE -->|sandboxRef| CS
```

`ClawSandbox` is the unit of work; the other seven CRDs bind policy, identity, peers, or evaluation to it. You can build a complete deployment with just `ClawSandbox` + `ToolPolicy` + `InferencePolicy`; the rest are opt-in for richer scenarios.

Schema details in **[`docs/api/crd-reference.md`](api/crd-reference.md)**.

---

## 8. Cluster topology — what `azureclaw up` produces

```mermaid
flowchart TB
  subgraph RG["Resource group: azureclaw-&lt;name&gt;-rg"]
    ACR["ACR<br/>(your private registry)"]
    Foundry["Azure AI Foundry<br/>+ Content Safety"]
    KV["Key Vault<br/>(optional)"]
    subgraph AKS["AKS cluster (Workload Identity + OIDC issuer)"]
      subgraph Sys["azureclaw-system"]
        CtrlPod["controller"]
        GwPod["a2a-gateway"]
      end
      subgraph Mesh["agentmesh"]
        Relay["agentmesh-relay"]
        Reg["agentmesh-registry"]
      end
      subgraph Tenant1["azureclaw-prod-agent (one per sandbox)"]
        Pod1["ClawSandbox pod<br/>(agent + router + egress-guard)"]
      end
      subgraph TenantN["azureclaw-&lt;other&gt;"]
        PodN["…"]
      end
    end
  end

  CtrlPod -.->|reconciles| Tenant1
  CtrlPod -.->|reconciles| TenantN
  Pod1 -->|encrypted| Relay
  PodN -->|encrypted| Relay
  Pod1 -->|HTTPS, WI| Foundry
  PodN -->|HTTPS, WI| Foundry
  GwPod -->|in-cluster| Pod1

  classDef sys fill:#e6f0ff,stroke:#0078d4
  classDef tenant fill:#f0fff0,stroke:#2a9d2a
  classDef mesh fill:#fff0f5,stroke:#9933cc
  class Sys sys
  class Tenant1 tenant
  class TenantN tenant
  class Mesh mesh
```

**Three classes of namespace:** `azureclaw-system` (the control plane, one per cluster), `agentmesh` (the relay/registry, one per cluster), and one tenant namespace per `ClawSandbox`. NetworkPolicy isolates them; the controller has Cluster-scoped RBAC; everything else is namespace-scoped.

---

## 9. Trust boundaries

Where each layer's authority ends.

```mermaid
flowchart TB
  subgraph Trust1["Trusted: cluster operator"]
    Ctrl["controller"]
    GW["A2A gateway"]
    Relay["AgentMesh relay/registry"]
  end
  subgraph Trust2["Trusted: per-pod (one fault domain per sandbox)"]
    Router["inference-router"]
    EG["egress-guard"]
  end
  subgraph Untrust["Untrusted: anything inside the agent container"]
    Agent["agent runtime + plugins + LLM output"]
  end

  Untrust --> Trust2
  Trust2 --> Trust1
  Trust1 --> Ext["external (Foundry, peers)"]
```

We treat the agent as **adversarial** — anything that comes out of the model could be a prompt-injection payload, a plugin could be malicious, a sub-agent spawn could be hostile. The router is the trust boundary: it does not run model output; it enforces policy *on* model output. Every class of bug above the line is a security bug; bugs in the agent runtime are availability bugs.

---

## See also

- **[Architecture](architecture.md)** — the prose explanation.
- **[Security model](security.md)** — per-layer guarantees.
- **[STRIDE threat model](security/stride.md)**.
- **[Blueprints](blueprints/00-index.md)** — five reference deployment shapes built from these primitives.
