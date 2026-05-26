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
    Secret["mounted secret<br/>(provider credential)"]
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
  subgraph Cluster["AKS cluster — namespace: kars-&lt;name&gt;"]
    subgraph Pod["KarsSandbox pod"]
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
  Router -->|"WS, opaque ciphertext (agent-sourced)"| Mesh
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
  participant WI as Workload Identity
  participant Foundry as Foundry model<br/>(+ inline Content Safety)

  Agent->>Router: POST /v1/chat (prompt)
  Router->>Gov: allow? token budget? rate?
  Gov-->>Router: allow + decision audit
  Router->>WI: exchange SA token → AAD
  WI-->>Router: bearer token
  alt provider == Foundry / Azure OpenAI
    Router->>Foundry: POST /openai/… + bearer<br/>(Content Safety enforced server-side)
    Foundry-->>Router: completion + prompt_filter_results
    Router->>Router: parse prompt_filter_results — block if jailbreak / category > threshold
  else provider == Copilot or GitHub Models
    Router->>Foundry: POST upstream + bearer<br/>(no inline Content Safety returned)
    Foundry-->>Router: completion (no prompt_filter_results)
  end
  Router-->>Agent: completion
  Note over Router: audit record:<br/>hash-chained, append-only<br/>(detection, not signing)
```

The agent has no direct path to **Foundry**, **WI**, or the audit
chain. The router brokers all of them. **Content Safety** is enforced
*inside* the Foundry call — the router does **not** make a separate
roundtrip; it parses the `prompt_filter_results` field that Foundry
returns inline and blocks/audits accordingly. On GitHub Copilot and
GitHub Models providers, inline filters are not returned, so this
step is a no-op (documented in `cli-reference.md` under `kars dev`).
The audit record is hash-chained for tamper-*detection*; cryptographic
signing of the chain head is on the roadmap (see [security.md](security.md#the-headline-guarantees)).

---

## 4. The mesh — encrypted inter-agent messaging

Two Kars agents in (possibly) different clusters that need to talk. The Signal-Protocol session (X3DH key agreement, Double Ratchet, KNOCK trust evaluation) lives **entirely inside the agent process** via `@microsoft/agent-governance-sdk`. The router is a transparent WebSocket bridge to the AgentMesh relay — it forwards opaque ciphertext, never holds a session key, and cannot decrypt. The relay is the same: ciphertext in, ciphertext out.

```mermaid
sequenceDiagram
  autonumber
  participant A as Alice agent<br/>(holds Signal session)
  participant Ar as Alice router<br/>(WS bridge only)
  participant Reg as AgentMesh registry
  participant Rel as AgentMesh relay
  participant Br as Bob router<br/>(WS bridge only)
  participant B as Bob agent<br/>(holds Signal session)

  Note over A,B: Setup (once per agent)
  A->>A: generate Ed25519 + X25519 + prekeys
  A->>Ar: PUT /agt/registry/agents/alice (signed prekey bundle)
  Ar->>Reg: forward (HTTPS)
  B->>B: generate identity + prekeys
  B->>Br: PUT /agt/registry/agents/bob
  Br->>Reg: forward

  Note over A,B: Session establishment + send
  A->>Ar: GET /agt/registry/agents/bob/prekeys
  Ar->>Reg: forward
  Reg-->>Ar: signed prekeys
  Ar-->>A: signed prekeys
  A->>A: X3DH → shared secret · init Double Ratchet · encrypt
  A->>Ar: WS /agt/relay → KNOCK + ciphertext
  Ar->>Rel: WS bytes (opaque)
  Rel->>Br: forward ciphertext (relay sees nothing)
  Br->>B: WS bytes (opaque)
  B->>B: trust score check (AGT_TRUST_THRESHOLD) · X3DH responder · decrypt
  alt accept
    B-->>Br: ratcheted ciphertext (reply)
    Br-->>Rel: WS bytes (opaque)
    Rel-->>Ar: forward
    Ar-->>A: WS bytes (opaque)
    A->>A: decrypt with current ratchet key
  else deny
    B-->>Rel: KNOCK denied (audit)
  end
```

**Session ownership:** the SDK is loaded by the agent's runtime (e.g., `runtimes/openai-agents/.../mesh.py::MeshClient`, the OpenClaw plugin), under UID 1000 in its own container. The router (UID 1001) runs `inference-router/src/routes/mesh.rs::relay_websocket_bridge` — pure byte-shuffling, no crypto. A compromise of the router (or the relay) leaks routing metadata only.

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
    GW["A2A gateway (Rust)<br/>verifies caller identity<br/>routes to A2AAgent CRD<br/>audit · rate limit · CS"]
    subgraph NS["kars-prod-agent"]
      Pod["KarsSandbox pod<br/>(agent + router)"]
    end
  end

  Peer -->|signed AgentCard + payload| Ing
  Ing --> GW
  GW -->|in-cluster only| Pod
  GW -.->|reject<br/>untrusted caller| Peer
```

The A2A gateway is the only inbound public surface. Every request gets the same content-safety, rate-limit, and audit treatment as outbound traffic.

> **Verifier status.** Today caller identity is established via the `X-A2A-Agent-Subject` header set by the upstream mTLS layer; AgentCard signature verification (`kars_a2a_core::verify_inbound_card`) ships as a library and is unit-tested, but wiring it as an axum layer inside the gateway is tracked in the [roadmap](roadmap.md). See [A2A gateway](architecture/a2a-gateway.md).

---

## 6. Control plane — what the controller does

```mermaid
flowchart LR
  User["operator / CLI / GitOps"]
  CRD[("9 CRDs<br/>KarsSandbox · A2AAgent · McpServer<br/>ToolPolicy · InferencePolicy<br/>KarsMemory · KarsEval · TrustGraph<br/>EgressApproval")]
  Ctrl["kars-controller<br/>(kube-rs)"]

  User -->|kubectl apply / kars cli| CRD
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

The controller is a vanilla kube-rs reconciler. It owns the nine user-facing CRDs (plus the controller-internal `KarsPairing`), watches them, and produces the boring Kubernetes objects that make a sandbox real. The CRD `status.conditions` chain is the operator-facing source of truth; every condition is documented in **[`docs/api/conditions.md`](api/conditions.md)**.

---

## 7. CRD relationships

How the nine CRDs reference each other. Arrow labels show the **actual** field path on the spec (camelCase as serialized).

```mermaid
flowchart TB
  CS["KarsSandbox<br/>(the agent)"]
  TP["ToolPolicy<br/>(allow / deny / approval)"]
  IP["InferencePolicy<br/>(model · tokens · region)"]
  CM["KarsMemory<br/>(memory store binding)"]
  Mcp["McpServer<br/>(allowed MCP backends)"]
  A2A["A2AAgent<br/>(public-ingress endpoint)"]
  TG["TrustGraph<br/>(mesh trust topology)"]
  CE["KarsEval<br/>(reproducible eval run)"]
  EA["EgressApproval<br/>(TTL-bounded extra hosts)"]

  CS -->|spec.inferenceRef| IP
  CS -->|spec.memoryRef| CM
  CS -->|spec.governance.toolPolicyRef| TP
  CS -->|spec.governance.mcpServerRefs| Mcp
  A2A -->|spec.policyRefs.toolPolicy| TP
  CE -->|spec.targetSandboxRef| CS
  EA -->|spec.sandbox| CS
  TG -.->|projected cluster-wide<br/>by controller| CS
```

`KarsSandbox` is the unit of work; the other CRDs bind policy, identity, peers, evaluation, or break-glass egress to it. You can build a complete deployment with just `KarsSandbox` + `ToolPolicy` + `InferencePolicy`; the rest are opt-in for richer scenarios.

`TrustGraph` is the one cluster-scoped CRD: the controller projects its edges into every sandbox namespace as a ConfigMap (`/etc/kars/trustgraph/graph.json`). It is not referenced by name from a `KarsSandbox` spec — it applies cluster-wide. **Router-side mesh-admission gating** against the projected graph (refuse to bridge a WS for an edge not in the graph) is tracked in the [roadmap](roadmap.md). This is not KNOCK gating — KNOCK lives inside the Signal session the agent owns end-to-end and the router never sees it. Today the router keeps a post-decision trust-score map populated from KNOCK outcomes the agent reports out-of-band, for audit and rate-limit purposes only (see CRD reference §TrustGraph).

Schema details in **[`docs/api/crd-reference.md`](api/crd-reference.md)**.

---

## 8. Cluster topology — what `kars up` produces

```mermaid
flowchart TB
  subgraph RG["Resource group: kars-&lt;name&gt;-rg"]
    ACR["ACR<br/>(your private registry)"]
    Foundry["Azure AI Foundry<br/>+ Content Safety"]
    KV["Key Vault<br/>(optional)"]
    subgraph AKS["AKS cluster (Workload Identity + OIDC issuer)"]
      subgraph Sys["kars-system"]
        CtrlPod["controller"]
        GwPod["a2a-gateway"]
      end
      subgraph Mesh["agentmesh"]
        Relay["agentmesh-relay"]
        Reg["agentmesh-registry"]
      end
      subgraph Tenant1["kars-prod-agent (one per sandbox)"]
        Pod1["KarsSandbox pod<br/>(agent + router + egress-guard)"]
      end
      subgraph TenantN["kars-&lt;other&gt;"]
        PodN["…"]
      end
    end
  end

  CtrlPod -.->|reconciles| Tenant1
  CtrlPod -.->|reconciles| TenantN
  Pod1 -->|WS, opaque ciphertext| Relay
  PodN -->|WS, opaque ciphertext| Relay
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

**Three classes of namespace:** `kars-system` (the control plane, one per cluster), `agentmesh` (the relay/registry, one per cluster), and one tenant namespace per `KarsSandbox`. NetworkPolicy isolates them; the controller has Cluster-scoped RBAC; everything else is namespace-scoped.

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
