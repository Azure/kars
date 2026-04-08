# AzureClaw — Architecture & Flow Diagrams

Visual reference for AzureClaw's core flows: sandbox architecture, agent lifecycle, inter-agent communication, inference routing, and egress control.

---

## 1. Sandbox Pod Architecture

The fundamental building block — every agent runs in this pod structure.

```mermaid
graph TB
    subgraph pod["Pod: azureclaw-‹name›"]
        direction TB

        subgraph init["Init Container: egress-guard (UID 0)"]
            ipt["iptables rules:<br/>UID 1000 → localhost + DNS only<br/>TCP 80/443 → REDIRECT :8444"]
        end

        subgraph oc["Container: openclaw (UID 1000)"]
            gw["OpenClaw Gateway<br/>:18789"]
            plugin["AzureClaw Plugin<br/>+ Foundry Skills"]
            agt_client["AgentMesh SDK<br/>(Signal Protocol)"]
        end

        subgraph ir["Container: inference-router (UID 1001)"]
            api["/v1/chat/completions<br/>/v1/responses<br/>/v1/images/generations<br/>:8443"]
            proxy["Forward Proxy<br/>:8444"]
            safety["Content Safety<br/>+ Token Budget"]
            blocklist["Domain Blocklist<br/>51K+ domains"]
            spawn_ep["/sandbox/spawn<br/>/agt/trust"]
            metrics_ep["Prometheus<br/>:9090"]
            policy["AGT PolicyEngine"]
            trust_store["Trust Store<br/>/tmp/agt/"]
            audit["Audit Chain<br/>(SHA-256 Merkle)"]
        end
    end

    subgraph volumes["Volumes"]
        v1["sandbox-data<br/>(emptyDir)"]
        v2["tmp<br/>(Memory, 1Gi)"]
        v3["admin-token<br/>(Secret)"]
        v4["blocklist-seed<br/>(ConfigMap)"]
        v5["agt-policy<br/>(ConfigMap)"]
    end

    init --> oc
    init --> ir
    oc --- v1
    oc --- v2
    ir --- v3
    ir --- v4
    ir --- v5

    style init fill:#f9f,stroke:#333,stroke-width:1px
    style oc fill:#4a9eff,stroke:#333,stroke-width:2px,color:#fff
    style ir fill:#ff6b35,stroke:#333,stroke-width:2px,color:#fff
```

### Network Access Matrix

```mermaid
graph LR
    subgraph "UID 1000 (Agent)"
        A[openclaw]
    end

    subgraph "UID 1001 (Router)"
        R[inference-router]
    end

    A -->|"localhost only"| R
    A -.->|"❌ BLOCKED"| IMDS["IMDS<br/>169.254.169.254"]
    A -.->|"❌ BLOCKED"| Internet["Internet"]
    A -->|"TCP 80/443<br/>REDIRECT"| FP["Forward Proxy<br/>:8444"]

    R -->|"✅ Workload Identity"| IMDS
    R -->|"✅ Inference"| Foundry["Azure AI Foundry"]
    R -->|"✅ Mesh"| Relay["AgentMesh Relay"]
    R -->|"✅ Governed"| Internet

    style A fill:#4a9eff,color:#fff
    style R fill:#ff6b35,color:#fff
    style IMDS fill:#ffd700,color:#000
    style Internet fill:#e74c3c,color:#fff
```

---

## 2. Agent Creation Flow (`azureclaw add`)

```mermaid
sequenceDiagram
    actor User
    participant CLI as azureclaw CLI
    participant Azure as Azure AD
    participant K8s as K8s API Server
    participant Ctrl as AzureClaw Controller
    participant NS as Namespace: azureclaw-‹name›

    User->>CLI: azureclaw add ‹name› --model gpt-4.1 --isolation enhanced

    rect rgb(240, 248, 255)
        Note over CLI,Azure: Step 1: Azure Identity
        CLI->>Azure: az identity federated-credential create
        Azure-->>CLI: Federated credential linked
    end

    rect rgb(245, 255, 245)
        Note over CLI,K8s: Step 2: K8s Secrets
        CLI->>K8s: Create Secret ‹name›-credentials<br/>(channel tokens, plugin API keys)
        K8s-->>CLI: Secret created
    end

    rect rgb(255, 245, 238)
        Note over CLI,Ctrl: Step 3: CRD + Reconciliation
        CLI->>K8s: kubectl apply ClawSandbox CRD
        K8s->>Ctrl: Watch event: ClawSandbox created

        Ctrl->>NS: Create Namespace
        Ctrl->>NS: Create ServiceAccount (Workload Identity)
        Ctrl->>NS: Create ClusterRoleBinding (spawner)
        Ctrl->>NS: Create Secrets (gateway-token, admin-token)
        Ctrl->>NS: Create NetworkPolicy (default-deny + allowlist)
        Ctrl->>NS: Create ConfigMap (blocklist seed)
        Ctrl->>NS: Create ConfigMap (AGT policy)
        Ctrl->>NS: Create Deployment (3 containers)
        Ctrl->>NS: Create Service (mesh DNS)
        Ctrl->>NS: Create CronJob (blocklist refresh)
    end

    rect rgb(248, 240, 255)
        Note over CLI,NS: Step 4: Wait for Ready
        loop Every 2s (max 120s)
            CLI->>K8s: Get pod status
            K8s-->>CLI: containerStatuses
        end
        NS-->>CLI: All containers Ready ✓
    end

    CLI-->>User: ✅ Agent running<br/>azureclaw connect ‹name›
```

---

## 3. Controller Reconciliation — Resources Created

```mermaid
graph TD
    CRD["ClawSandbox CRD<br/>(azureclaw-system)"] -->|"triggers"| Ctrl["AzureClaw Controller<br/>(Rust / kube-rs)"]

    Ctrl --> NS["Namespace<br/>azureclaw-‹name›"]
    Ctrl --> SA["ServiceAccount<br/>sandbox"]
    Ctrl --> CRB["ClusterRoleBinding<br/>azureclaw-spawner-‹name›"]
    Ctrl --> S1["Secret<br/>gateway-token (32 char)"]
    Ctrl --> S2["Secret<br/>router-admin-token (64 char)"]
    Ctrl --> NP["NetworkPolicy<br/>sandbox-policy"]
    Ctrl --> DEP["Deployment<br/>1 replica, 3 containers"]
    Ctrl --> SVC["Service<br/>:8443 (mesh DNS)"]
    Ctrl --> CM1["ConfigMap<br/>blocklist seed"]
    Ctrl --> CM2["ConfigMap<br/>AGT policy profile"]
    Ctrl --> CJ["CronJob<br/>blocklist refresh (6h)"]
    Ctrl --> FC["Azure Federated Credential"]

    DEP --> IC["Init: egress-guard"]
    DEP --> C1["Container: openclaw<br/>UID 1000 · :18789"]
    DEP --> C2["Container: inference-router<br/>UID 1001 · :8443 :8444 :9090<br/>+ native AGT governance"]

    NP --> NP1["Egress: DNS ✅"]
    NP --> NP2["Egress: IMDS ✅ (router only)"]
    NP --> NP3["Egress: HTTPS ✅ (no private IPs)"]
    NP --> NP4["Egress: Mesh ✅ (other sandboxes)"]
    NP --> NP5["Egress: Relay/Registry ✅"]
    NP --> NP6["Ingress: deny all"]

    style CRD fill:#9b59b6,color:#fff
    style Ctrl fill:#e67e22,color:#fff
    style DEP fill:#3498db,color:#fff
    style NP fill:#e74c3c,color:#fff
```

---

## 4. Sub-Agent Spawn Flow

```mermaid
sequenceDiagram
    participant Parent as Parent Agent<br/>(openclaw)
    participant Plugin as AzureClaw Plugin
    participant Router as Inference Router<br/>(:8443)
    participant K8s as K8s API Server
    participant Ctrl as Controller
    participant Child as Child Agent Pod
    participant Registry as AgentMesh Registry

    Parent->>Plugin: azureclaw_spawn("analyst", "gpt-4.1", governance=true)

    rect rgb(255, 245, 238)
        Note over Plugin,K8s: Step 1: Create Sub-Agent
        Plugin->>Plugin: Build trusted_peers list<br/>(parent AMID + siblings)
        Plugin->>Router: POST /sandbox/spawn<br/>{name, model, governance, trusted_peers}
        Router->>K8s: Create ClawSandbox CRD<br/>(inherits parent isolation level)
        K8s->>Ctrl: Watch event
        Ctrl->>Child: Reconcile → create namespace,<br/>deployment, networkpolicy...
    end

    rect rgb(240, 248, 255)
        Note over Plugin,Registry: Step 2: Wait for Ready + Discovery
        loop Every 1s (max 45s)
            Plugin->>Router: GET /sandbox/analyst/status
            Router-->>Plugin: phase: Pending → Running
            Plugin->>Router: GET /agt/registry/search?capability=analyst
            Router->>Registry: Forward search query
            Registry-->>Router: [{amid, display_name, tier}]
            Router-->>Plugin: AMID discovered
        end
        Plugin->>Plugin: Cache AMID in nameToAmid map
    end

    Plugin-->>Parent: ✅ Sub-agent "analyst" running<br/>Ready for mesh communication
```

---

## 5. E2E Encrypted Agent-to-Agent Communication

```mermaid
sequenceDiagram
    participant PA as Parent Agent
    participant PP as Parent Plugin
    participant PR as Parent Router<br/>(:8443)
    participant Relay as AgentMesh Relay<br/>(WebSocket :8765)
    participant CR as Child Router<br/>(:8443)
    participant CP as Child Plugin
    participant CA as Child Agent

    Note over PA,CA: Signal Protocol: X3DH Key Exchange + Double Ratchet

    rect rgb(255, 248, 240)
        Note over PP,CP: First Contact: KNOCK + X3DH Handshake
        PP->>Relay: KNOCK {from: parentAMID, to: childAMID}
        Relay->>CP: Forward KNOCK
        CP->>CP: Check trust score ≥ 500?
        CP-->>Relay: ACCEPT + prekey bundle
        Relay-->>PP: Forward ACCEPT
        PP->>PP: X3DH: compute shared secret<br/>(ECDH + HKDF → session key)
        Note over PP,CP: 🔐 Session established<br/>Forward secrecy via Double Ratchet
    end

    rect rgb(240, 255, 240)
        Note over PA,CA: Encrypted Task Delegation
        PA->>PP: mesh_send("analyst", "Analyze market trends")
        PP->>PP: Encrypt with session key<br/>(Double Ratchet)
        PP->>Relay: Encrypted envelope<br/>(relay cannot decrypt)
        Relay->>CP: Route by AMID
        CP->>CP: Decrypt with session key
        CP->>CA: Deliver task: "Analyze market trends"
    end

    rect rgb(240, 240, 255)
        Note over PA,CA: Encrypted Response
        CA->>CP: Result: "Market analysis complete..."
        CP->>CP: Encrypt response
        CP->>Relay: Encrypted envelope
        Relay->>PP: Route by AMID
        PP->>PP: Decrypt response
        PP->>PA: Deliver: "Market analysis complete..."
        PP->>PR: POST /agt/trust (reputation +0.9)
    end
```

### Trust Gate Decision Flow

```mermaid
flowchart TD
    KNOCK["Incoming KNOCK<br/>from unknown agent"] --> LOOKUP["Look up sender AMID<br/>in Trust Store"]
    LOOKUP --> REG["Query Registry:<br/>reputation, tier"]
    REG --> CALC["Calculate effective score:<br/>registry × 0.7 + local × 0.3"]
    CALC --> BONUS{"Parent-verified<br/>peer?"}
    BONUS -->|"Yes"| ADD["+100 affinity bonus"]
    BONUS -->|"No"| CHECK
    ADD --> CHECK{"Score ≥ threshold<br/>(default 500)?"}
    CHECK -->|"✅ Yes"| ACCEPT["Accept KNOCK<br/>→ X3DH handshake<br/>→ Establish session"]
    CHECK -->|"❌ No"| REJECT["Reject KNOCK<br/>→ Log to audit chain<br/>→ Block for 60s"]

    style ACCEPT fill:#2ecc71,color:#fff
    style REJECT fill:#e74c3c,color:#fff
    style CALC fill:#f39c12,color:#fff
```

---

## 6. Inference Request Flow

```mermaid
sequenceDiagram
    participant Agent as Agent<br/>(UID 1000)
    participant Router as Inference Router<br/>(UID 1001)
    participant IMDS as IMDS<br/>(169.254.169.254)
    participant Foundry as Azure AI Foundry

    Agent->>Router: POST /v1/chat/completions<br/>{model, messages, stream}

    rect rgb(255, 240, 240)
        Note over Router: Gate 1: Governance Policy
        Router->>Router: PolicyEngine.evaluate()<br/>{action: "inference:chat_completions"}
        Note right of Router: ✅ allow (or ❌ 403 deny)
    end

    rect rgb(255, 248, 220)
        Note over Router: Gate 2: Token Budget
        Router->>Router: Check daily budget<br/>(TOKEN_BUDGET_DAILY)
        Router->>Router: Check per-request limit<br/>(TOKEN_BUDGET_PER_REQUEST)
        Note right of Router: ❌ 429 if exceeded
    end

    rect rgb(240, 248, 255)
        Note over Router,IMDS: Gate 3: Authentication
        Router->>IMDS: GET /metadata/identity/oauth2/token<br/>(Workload Identity)
        IMDS-->>Router: Bearer token<br/>(agent never sees this)
    end

    rect rgb(240, 255, 240)
        Note over Router,Foundry: Gate 4: Inference + Safety
        Router->>Foundry: POST /chat/completions<br/>+ Bearer token<br/>+ Content Safety (DefaultV2)
        Foundry->>Foundry: Prompt Shields<br/>(jailbreak detection)
        Foundry->>Foundry: Content filters<br/>(hate, violence, self-harm, sexual)
        Foundry-->>Router: Response + filter annotations
    end

    Router->>Router: Record token usage to budget
    Router->>Router: Parse safety annotations<br/>(trust penalty if violations)
    Router-->>Agent: Response (filtered)
```

---

## 7. Egress Control Flow

```mermaid
flowchart TD
    REQ["Agent (UID 1000)<br/>curl https://api.github.com"] --> IPT{"iptables<br/>NAT REDIRECT"}
    IPT -->|"TCP 443 → :8444"| FP["Forward Proxy<br/>(inference-router :8444)"]
    FP --> EXTRACT["Extract domain<br/>from CONNECT request"]
    EXTRACT --> BL{"Domain in<br/>blocklist?<br/>(51K+ domains)"}

    BL -->|"❌ Yes"| BLOCK["403 Forbidden<br/>Log: blocked domain<br/>+ reason (OISD/URLhaus/TLD)"]
    BL -->|"No"| TLD{"High-risk TLD?<br/>(.tk .ml .ga .cf .gq)"}

    TLD -->|"❌ Yes"| BLOCK
    TLD -->|"No"| MODE{"Egress mode?"}

    MODE -->|"Learn"| LOG["Log domain access<br/>→ operator review"]
    LOG --> TUNNEL

    MODE -->|"Enforce"| AL{"In allowlist?"}
    AL -->|"✅ Yes"| TUNNEL["Create TCP tunnel<br/>→ relay to destination"]
    AL -->|"❌ No"| PENDING["PendingApproval<br/>→ operator notified"]

    TUNNEL --> RELAY["Bidirectional relay<br/>agent ↔ destination<br/>(1h max, 256 concurrent)"]

    style BLOCK fill:#e74c3c,color:#fff
    style PENDING fill:#f39c12,color:#fff
    style TUNNEL fill:#2ecc71,color:#fff
    style LOG fill:#3498db,color:#fff
```

### Learn → Enforce Lifecycle

```mermaid
stateDiagram-v2
    [*] --> LearnMode: azureclaw add ‹name›

    LearnMode: 🔍 Learn Mode
    LearnMode: All domains logged
    LearnMode: Blocklist still enforced
    LearnMode: Operator observes traffic

    Review: 📋 Operator Review
    Review: azureclaw egress ‹name› --learned
    Review: See all accessed domains
    Review: Approve / Deny each

    EnforceMode: 🔒 Enforce Mode
    EnforceMode: Only approved domains pass
    EnforceMode: Everything else blocked
    EnforceMode: New domains → PendingApproval

    LearnMode --> Review: Operator inspects learned domains
    Review --> Review: Approve (a) / Deny (d) domains
    Review --> EnforceMode: azureclaw egress --enforce
    EnforceMode --> LearnMode: azureclaw egress --learn
```

---

## 8. Full Deployment Flow (`azureclaw up`)

```mermaid
flowchart TD
    UP["azureclaw up<br/>--name my-agent --model gpt-4.1"] --> RG

    subgraph azure["Azure Resources"]
        RG["Resource Group"] --> AKS["AKS Cluster<br/>(system + sandbox pools)"]
        RG --> ACR["Container Registry<br/>(ACR)"]
        RG --> AI["AI Foundry Project<br/>+ AI Services"]
        RG --> MI["Managed Identity<br/>(Workload Identity)"]
        RG --> ST["Storage Account"]
    end

    subgraph images["Container Images → ACR"]
        ACR --> I1["azureclaw-controller:latest"]
        ACR --> I2["azureclaw-inference-router:latest"]
        ACR --> I3["openclaw-sandbox:latest"]
        ACR --> I4["agentmesh-relay:latest"]
        ACR --> I5["agentmesh-registry:latest"]
        ACR --> I6["postgres:16-alpine"]
    end

    subgraph k8s["K8s Cluster Resources"]
        AKS --> CRD["ClawSandbox CRD"]
        AKS --> CTRL["Controller Deployment"]
        AKS --> SEC["Seccomp DaemonSet<br/>(azureclaw-strict profile)"]
        AKS --> MESH["AgentMesh Namespace"]
        MESH --> PG["PostgreSQL"]
        MESH --> RL["Relay (:8765)"]
        MESH --> REG["Registry (:8080)"]
        MESH --> DB_SEC["DB Credentials Secret<br/>(auto-generated)"]
        AKS --> GADGET["Inspektor Gadget<br/>(eBPF, optional)"]
    end

    subgraph sandbox["First Sandbox"]
        CRD --> SB["ClawSandbox: my-agent"]
        SB --> NS["Namespace + Pod<br/>+ NetworkPolicy<br/>+ Secrets + ConfigMaps"]
    end

    style UP fill:#9b59b6,color:#fff
    style AKS fill:#0078d4,color:#fff
    style ACR fill:#0078d4,color:#fff
    style AI fill:#0078d4,color:#fff
    style CTRL fill:#e67e22,color:#fff
    style SB fill:#2ecc71,color:#fff
```

---

## 9. Multi-Agent Topology

What a production mesh looks like with parent + sub-agents.

```mermaid
graph TB
    subgraph cluster["AKS Cluster"]
        subgraph ns1["ns: azureclaw-orchestrator"]
            P["🔱 orchestrator<br/>gpt-4.1 · enhanced<br/>UID 1000 + Router + AGT"]
        end

        subgraph ns2["ns: azureclaw-researcher"]
            C1["📊 researcher<br/>gpt-4.1 · enhanced"]
        end

        subgraph ns3["ns: azureclaw-analyst"]
            C2["🔍 analyst<br/>gpt-5-mini · enhanced"]
        end

        subgraph ns4["ns: azureclaw-writer"]
            C3["✍️ writer<br/>gpt-4.1 · enhanced"]
        end

        subgraph mesh["ns: agentmesh"]
            Relay["Relay<br/>WebSocket :8765"]
            Registry["Registry<br/>REST :8080"]
            PG["PostgreSQL<br/>:5432"]
        end
    end

    P ===|"🔐 E2E Encrypted<br/>Signal Protocol"| Relay
    C1 ===|"🔐"| Relay
    C2 ===|"🔐"| Relay
    C3 ===|"🔐"| Relay

    C1 -.->|"register AMID<br/>+ prekeys"| Registry
    C2 -.->|"register"| Registry
    C3 -.->|"register"| Registry
    P -.->|"discover AMIDs"| Registry
    Registry --- PG

    P -->|"spawn"| C1
    P -->|"spawn"| C2
    P -->|"spawn"| C3

    style P fill:#e67e22,color:#fff,stroke-width:3px
    style C1 fill:#3498db,color:#fff
    style C2 fill:#3498db,color:#fff
    style C3 fill:#3498db,color:#fff
    style Relay fill:#9b59b6,color:#fff
    style Registry fill:#9b59b6,color:#fff
```

---

## 10. Defense-in-Depth Layers

```mermaid
graph TB
    subgraph L0["Layer 0: Azure Infrastructure"]
        AKS_SEC["AKS API IP restriction<br/>NSG · DDoS Protection"]
    end

    subgraph L1["Layer 1: Node OS"]
        OS["Azure Linux<br/>SELinux enforcing"]
    end

    subgraph L2["Layer 2: VM Isolation (optional)"]
        KATA["Kata Containers<br/>Cloud Hypervisor<br/>AMD SEV-SNP"]
    end

    subgraph L3["Layer 3: Container Hardening"]
        CH["Read-only rootfs<br/>Non-root (UID 1000)<br/>Drop ALL capabilities"]
    end

    subgraph L4["Layer 4: Kernel Confinement"]
        SECC["Seccomp: 175 allowed syscalls<br/>Blocks: ptrace, mount, bpf,<br/>unshare, kexec, chroot"]
    end

    subgraph L5["Layer 5: Network Segmentation"]
        NET["iptables UID guard<br/>NetworkPolicy per-namespace<br/>Forward proxy + blocklist"]
    end

    subgraph L6["Layer 6: Inference Safety"]
        INF["Content Safety (every call)<br/>Prompt Shields (jailbreak)<br/>Token budgets (daily + per-req)"]
    end

    subgraph L7["Layer 7: Zero Credentials"]
        CRED["Workload Identity via router<br/>Agent never sees tokens<br/>IMDS blocked for agent"]
    end

    subgraph L8["Layer 8: E2E Encryption"]
        E2E["Signal Protocol (X3DH + Ratchet)<br/>Trust-gated messaging<br/>Untrusted relay"]
    end

    subgraph L9["Layer 9: Behavioral Governance"]
        GOV["AGT policy evaluation<br/>Dynamic trust scoring<br/>Tamper-evident audit chain"]
    end

    L0 --> L1 --> L2 --> L3 --> L4 --> L5 --> L6 --> L7 --> L8 --> L9

    style L0 fill:#ecf0f1,stroke:#bdc3c7
    style L1 fill:#dfe6e9,stroke:#b2bec3
    style L2 fill:#ffeaa7,stroke:#fdcb6e
    style L3 fill:#fab1a0,stroke:#e17055
    style L4 fill:#ff7675,stroke:#d63031
    style L5 fill:#fd79a8,stroke:#e84393
    style L6 fill:#a29bfe,stroke:#6c5ce7
    style L7 fill:#74b9ff,stroke:#0984e3
    style L8 fill:#55efc4,stroke:#00b894
    style L9 fill:#81ecec,stroke:#00cec9
```
