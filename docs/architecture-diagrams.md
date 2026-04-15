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
        SECC["Seccomp: 219 allowed, 28 blocked<br/>Blocks: ptrace, mount, bpf,<br/>unshare, kexec, chroot"]
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

---

## 11. Cloud Handoff Flow (Dev → AKS)

LLM-driven agent migration from local Docker to AKS cloud. The LLM requests the handoff, the user confirms with a code, and the plugin orchestrates the transfer asynchronously — reporting live progress via emoji status updates.

### 11.1 End-to-End Sequence

```mermaid
sequenceDiagram
    autonumber
    participant User as 👤 User (webchat)
    participant LLM as 🤖 LLM
    participant Plugin as 🔌 Plugin (source)
    participant Router as 🛡️ Source Router
    participant K8s as ☸️ K8s API
    participant Ctrl as 🎛️ Controller
    participant Reg as 📋 Global Registry
    participant Relay as 📡 Relay (E2E)
    participant TPlugin as 🔌 Plugin (target)
    participant TRouter as 🛡️ Target Router

    Note over User,TRouter: ──── Phase 1: Two-Stage Confirmation Gate ────

    User->>LLM: "move to the cloud"
    LLM->>Plugin: azureclaw_handoff_request(direction=cloud)
    Plugin->>Router: POST /agt/handoff/pending
    Note right of Router: Rate limit: 1 req / 300s<br/>Token: random 8-hex, TTL 5min
    Router-->>Plugin: {confirmation_token, expires_in}
    Plugin-->>LLM: "confirm with code: f913b1f9"
    LLM-->>User: "🔄 To confirm, reply: f913b1f9"
    User->>LLM: "f913b1f9"
    LLM->>Plugin: azureclaw_handoff_confirm(code=f913b1f9)
    Plugin->>Router: POST /agt/handoff/confirm
    Note right of Router: Min 3s delay enforced<br/>Constant-time comparison
    Router-->>Plugin: {handoff_token 🔒, direction}

    Note over User,TRouter: ──── Phase 2: Async Background Orchestration ────
    Note over Plugin: Plugin returns immediately<br/>LLM polls handoff_status every 3-5s

    Plugin-->>LLM: "started — poll handoff_status"

    Note over Plugin,Router: Step 1: Snapshot (timeout: 60s)
    Plugin->>Router: POST /agt/handoff/snapshot
    Note right of Router: AES-256-GCM encrypted<br/>key = HKDF(SHA256(admin‖handoff))
    Router-->>Plugin: {blob, verification_hash, size_bytes}

    LLM->>Plugin: azureclaw_handoff_status
    Plugin-->>LLM: "📦 Snapshot ready (13.7 KB)"
    LLM-->>User: 📦 Snapshot ready (13.7 KB)

    Note over Plugin,Router: Step 2: Drain (timeout: 30s)
    Plugin->>Router: POST /agt/handoff/drain
    Note right of Router: Stop accepting new work<br/>⚠️ No undrain if aborted

    Note over Plugin,Ctrl: Step 3: Spawn AKS Target
    Plugin->>Router: POST /sandbox/spawn
    Note right of Router: {handoff: {mode: restore}}<br/>Dev mode bypasses Docker → K8s CRD
    Router->>K8s: Create ClawSandbox CRD
    Note right of K8s: labels: spawned-by=handoff<br/>governance.trustedPeers = source AMID<br/>governance.registryMode = global
    Ctrl->>K8s: Reconcile → Pod + NetworkPolicy
    Note right of Ctrl: Both openclaw AND router get:<br/>AGT_TRUSTED_PEERS, AGT_REGISTRY_MODE

    LLM->>Plugin: azureclaw_handoff_status
    Plugin-->>LLM: "🚀 CRD created, waiting for pod..."
    LLM-->>User: 🚀 Cloud target spawning...

    Note over Plugin,Reg: Step 4: Mesh Discovery (90s max)
    TPlugin->>Reg: Register AMID (Ed25519)
    loop Poll registry every 2s
        Plugin->>Reg: search for target AMID
    end
    Reg-->>Plugin: target AMID found ✓

    LLM->>Plugin: azureclaw_handoff_status
    Plugin-->>LLM: "🌐 Cloud target online"
    LLM-->>User: 🌐 Cloud target online

    Note over Plugin,TPlugin: Step 5: E2E State Transfer (5 retries × 2s)
    Plugin->>Relay: mesh_send(handoff_transfer, blob, secret, hash)
    Note over Relay: 🔐 Signal Protocol (X3DH + Double Ratchet)<br/>Relay is zero-knowledge
    Relay->>TPlugin: deliver encrypted message

    Note over TPlugin,TRouter: Step 5b: Target Restores State
    TPlugin->>TRouter: POST /agt/handoff/init
    TPlugin->>TRouter: POST /agt/handoff/restore {blob, secret}
    Note right of TRouter: Decrypt, decompress,<br/>sanitize chat (anti-injection),<br/>trust scores capped at 750
    TPlugin->>TRouter: POST /agt/handoff/verify {expected_hash}
    Note right of TRouter: SHA-256 integrity match

    Note over TPlugin,Plugin: Step 5c: Verification via E2E Mesh
    TPlugin->>Relay: mesh_send(handoff_verification)
    Note right of TPlugin: {matches, successor_amid,<br/>trust_scores_count, audit_entries_count}
    Relay->>Plugin: deliver verification ✓
    Note over Plugin: Filter: from_amid AND<br/>from_agent must BOTH match

    LLM->>Plugin: azureclaw_handoff_status
    Plugin-->>LLM: "✅ State verified — hash match"
    LLM-->>User: ✅ State verified

    Note over Plugin,Reg: Step 6: Identity Succession
    Plugin->>Reg: POST /registry/succession
    Note right of Reg: Ed25519 signed<br/>Copies reputation, marks predecessor Dormant

    Note over Plugin,Router: Step 7: Decommission
    Plugin->>Router: POST /agt/handoff/decommission
    Note right of Router: Dormant — keys preserved<br/>Ghost cleanup skips dormant agents

    LLM->>Plugin: azureclaw_handoff_status
    Plugin-->>LLM: "🎉 Handoff complete!"
    LLM-->>User: 🎉 I'm now running on AKS! Your keys are preserved for reverse handoff.
```

### 11.2 Handoff State Machine

The router tracks handoff phases. **Note:** phase ordering is enforced by convention in the plugin, not by the router — endpoints are currently callable in any order (documented improvement area).

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> PendingConfirmation: POST /pending
    PendingConfirmation --> Confirmed: POST /confirm (code match, ≥3s delay)
    PendingConfirmation --> Idle: timeout (5min) / cancel
    Confirmed --> Snapshotting: POST /snapshot
    Snapshotting --> Draining: POST /drain
    Draining --> Transferring: mesh_send(handoff_transfer)
    Transferring --> Restoring: POST /restore (on target)
    Restoring --> Verifying: POST /verify (on target)
    Verifying --> Succession: POST /registry/succession
    Succession --> Decommissioning: POST /decommission
    Decommissioning --> [*]

    Confirmed --> Aborted: POST /abort
    Snapshotting --> Aborted: POST /abort
    Draining --> Aborted: POST /abort or error
    Transferring --> Aborted: POST /abort
    Verifying --> Aborted: timeout (60s)
    Aborted --> [*]

    note right of Draining
        ⚠️ No undrain mechanism.
        Abort after drain leaves agent
        in drained state until restart.
    end note

    note right of Transferring
        ⚠️ If mesh transfer fails after spawn,
        orphaned CRD must be manually deleted:
        kubectl delete clawsandbox <name>
    end note
```

### 11.3 Security Model

```mermaid
%%{init: {'theme': 'default'}}%%
graph TB
    subgraph gate["Layer 1: Human-in-the-Loop Gate"]
        direction LR
        pending["POST /pending<br/>Rate limit: 1 req / 300s"]
        confirm["POST /confirm<br/>Min 3s delay, constant-time compare"]
        pending --> confirm
    end

    subgraph isolation["Layer 2: LLM Isolation"]
        direction LR
        token_iso["Handoff token stays in<br/>plugin memory — LLM never sees it"]
        admin_iso["Admin token read from<br/>/run/secrets/ (K8s mount)"]
        execute["LLM can REQUEST<br/>but never EXECUTE"]
    end

    subgraph auth["Layer 3: Three-Layer Endpoint Auth"]
        direction LR
        l1["Init: admin token<br/>(no localhost bypass)"]
        l2["Mutations: admin + handoff token<br/>(no localhost bypass)"]
        l3["Status: admin token<br/>(localhost bypass OK — read-only)"]
    end

    subgraph encryption["Layer 4: Double Encryption"]
        direction LR
        aes["Snapshot: AES-256-GCM<br/>key = HKDF-SHA256(admin‖handoff)"]
        signal["Transport: Signal Protocol<br/>X3DH + Double Ratchet per session"]
        relay_zk["Relay is zero-knowledge<br/>cannot decrypt payloads"]
    end

    subgraph injection["Layer 5: State Blob Hardening"]
        direction LR
        sanitize["Chat history sanitized<br/>(strip system prompts, JSON injection)"]
        trust_cap["Trust scores capped at 750<br/>(prevent score inflation)"]
        size_limit["50MB blob limit<br/>100 files max, 10MB/file"]
    end

    subgraph identity["Layer 6: Identity & Trust"]
        direction LR
        ed25519["Ed25519 keypairs<br/>AMIDs = public keys"]
        trusted["AGT_TRUSTED_PEERS<br/>K8s-injected to BOTH containers"]
        knock["KNOCK enforcement<br/>+500 bonus for trusted peers"]
        verify_both["Verification filter:<br/>from_amid OR from_agent mismatch → reject"]
    end

    subgraph infra["Layer 7: Infrastructure"]
        direction LR
        netpol["NetworkPolicy: default-deny<br/>mesh egress only"]
        readonly["Read-only rootfs<br/>seccomp + non-root"]
        kube["Kubeconfig: read-only mount<br/>dev mode only"]
        validated["registry_mode validated: local|global<br/>trusted_peers: control chars rejected"]
    end

    gate --> isolation --> auth --> encryption --> injection --> identity --> infra
```

### 11.4 Env Var Propagation (Controller → Containers)

Both containers in the sandbox pod receive governance env vars. This is critical for handoff — the router needs `AGT_REGISTRY_MODE` to gate handoff endpoints, and `AGT_TRUSTED_PEERS` for KNOCK authentication.

```mermaid
%%{init: {'theme': 'default'}}%%
graph LR
    CRD["ClawSandbox CRD<br/>governance spec"] --> Ctrl["Controller<br/>reconciler.rs"]

    Ctrl --> OC["OpenClaw Container"]
    Ctrl --> IR["Inference Router"]

    subgraph OC_env["OpenClaw Env Vars"]
        direction TB
        oc1["AGT_GOVERNANCE_ENABLED"]
        oc2["AGT_POLICY_PROFILE"]
        oc3["AGT_TRUST_THRESHOLD"]
        oc4["AGT_TRUSTED_PEERS ✅"]
        oc5["AGT_REGISTRY_MODE ✅"]
    end

    subgraph IR_env["Router Env Vars"]
        direction TB
        ir1["AGT_GOVERNANCE_ENABLED"]
        ir2["AGT_POLICY_PROFILE"]
        ir3["AGT_TRUST_THRESHOLD"]
        ir4["AGT_TRUSTED_PEERS ✅"]
        ir5["AGT_REGISTRY_MODE ✅"]
        ir6["AGT_MESH_NAMESPACE"]
        ir7["AGT_RELAY_URL"]
        ir8["AGT_REGISTRY_URL"]
    end

    OC --> OC_env
    IR --> IR_env

    subgraph validation["CRD Validation"]
        direction TB
        v1["trusted_peers: reject \\n \\r \\0"]
        v2["registry_mode: only local|global"]
    end

    CRD --> validation
    validation --> Ctrl
```

### 11.5 Two Orchestration Paths

There are two independent orchestration paths for handoff — both valid, serving different use cases:

```mermaid
%%{init: {'theme': 'default'}}%%
graph TB
    subgraph llm_path["LLM-Driven (plugin.ts — interactive webchat)"]
        direction TB
        L1["handoff_request tool<br/>→ POST /agt/handoff/pending"]
        L2["handoff_confirm tool<br/>→ POST /agt/handoff/confirm"]
        L3["_runHandoffOrchestration()<br/>runs async in background"]
        L4["handoff_status tool<br/>polled every 3-5s by LLM"]
        L1 --> L2 --> L3
        L3 -.-> L4
    end

    subgraph cli_path["CLI-Driven (handoff.ts — operator terminal)"]
        direction TB
        C1["azureclaw handoff ‹name›<br/>→ POST /agt/handoff/init"]
        C2["Direct snapshot + drain<br/>→ POST /snapshot, /drain"]
        C3["kubectl apply CRD<br/>+ port-forward + restore"]
        C4["Terminal progress bar<br/>(Stepper class)"]
        C1 --> C2 --> C3
        C3 -.-> C4
    end

    subgraph differences["Key Differences"]
        direction TB
        D1["LLM path: two-stage gate<br/>(pending → confirm with code)"]
        D2["CLI path: direct init<br/>(operator trusted, no gate)"]
        D3["LLM path: mesh transfer<br/>(E2E encrypted via relay)"]
        D4["CLI path: port-forward restore<br/>(direct HTTP to target)"]
    end

    llm_path --- differences
    cli_path --- differences
```

### 11.6 Trust Flow — Current vs Future

Currently agents register anonymously (trust score = 0), so `AGT_TRUSTED_PEERS` provides the +500 KNOCK bonus. With Entra OAuth deployed, agents get verified identities and real reputation.

```mermaid
%%{init: {'theme': 'default'}}%%
graph LR
    subgraph current["Current — Unauthenticated"]
        S1["Source AMID<br/>registry score: 0"] -->|KNOCK| T1["Target<br/>threshold: 500"]
        T1 -->|"+500 trusted_peers bonus"| A1["✅ 500 ≥ 500"]
    end

    subgraph future["Future — Entra OAuth"]
        S2["Source AMID<br/>Entra-verified"] -->|KNOCK| T2["Target<br/>threshold: 500"]
        T2 -->|"registry reputation: 800"| A2["✅ 800 ≥ 500"]
        Note1["No trusted_peers needed<br/>Real identity = real trust"]
    end
```

### 11.7 Error Recovery & Known Limitations

```mermaid
%%{init: {'theme': 'default'}}%%
graph TB
    subgraph failure_modes["Failure Modes"]
        F1["Mesh send fails<br/>(5 retries exhausted)"]
        F2["Target doesn't register<br/>(90s timeout)"]
        F3["Verification timeout<br/>(60s)"]
        F4["Restore decrypt fails<br/>(wrong key / tampered blob)"]
        F5["Abort after drain"]
    end

    subgraph recovery["Current Recovery"]
        R1["Abort → token revoked,<br/>phase reset"]
        R2["Abort → phase reset,<br/>target pod orphaned ⚠️"]
        R3["Status = partial,<br/>target may still restore"]
        R4["Target sends error<br/>via mesh → abort"]
        R5["Agent stuck in drained<br/>state until restart ⚠️"]
    end

    subgraph improvements["Planned Improvements"]
        I1["Auto-cleanup orphaned CRDs<br/>on abort"]
        I2["POST /agt/handoff/resume<br/>to cancel drain"]
        I3["Router enforces phase<br/>ordering (state machine)"]
    end

    F1 --> R1
    F2 --> R2
    F3 --> R3
    F4 --> R4
    F5 --> R5

    R2 --> I1
    R5 --> I2
    recovery --> I3
```

---

## 12. Bidirectional Handoff with Sub-Agents

Full roundtrip handoff: local Docker ↔ AKS cloud, including sub-agent lifecycle (snapshot, destroy, re-spawn, workspace inject, task resume). The local Docker parent is the permanent "home base" — everything else is ephemeral.

### 12.1 Agent Lifecycle Across Handoff

```mermaid
%%{init: {'theme': 'default'}}%%
graph LR
    subgraph local["🐳 Local Docker — Home Base"]
        LP["🏠 Parent Agent<br/>permanent — goes dormant,<br/>never deleted"]
        LS1["Sub-Agent 1<br/>ephemeral"]
        LS2["Sub-Agent 2<br/>ephemeral"]
    end

    subgraph cloud["☁️ AKS Cloud — Ephemeral"]
        CP["Parent Agent<br/>created by forward handoff,<br/>CRD deleted on reverse"]
        CS1["Sub-Agent 1<br/>re-spawned from snapshot"]
        CS2["Sub-Agent 2<br/>re-spawned from snapshot"]
    end

    LP -->|"forward: snapshot + spawn"| CP
    LS1 -.->|"destroyed"| CS1
    LS2 -.->|"destroyed"| CS2
    CP -->|"reverse: snapshot + wake"| LP
    CS1 -.->|"CRD deleted"| LS1
    CS2 -.->|"CRD deleted"| LS2
```

**Key principle:** Sub-agents are never migrated — they are destroyed and re-spawned. Only the parent's snapshot (which includes sub-agent definitions and workspace tars) crosses the boundary.

### 12.2 Forward Handoff: Local → Cloud (with Sub-Agents)

CLI-driven path (`azureclaw handoff <name> --to cloud`). 7 stepper steps.

```mermaid
sequenceDiagram
    autonumber
    participant CLI as 🖥️ CLI (handoff.ts)
    participant SrcR as 🛡️ Source Router<br/>(Docker)
    participant Sub as 🐳 Docker Sub-Agents
    participant K8s as ☸️ K8s API
    participant Ctrl as 🎛️ Controller
    participant TgtR as 🛡️ Target Router<br/>(AKS)
    participant Plugin as 🔌 Target Plugin<br/>(AKS)
    participant Reg as 📋 Registry
    participant Relay as 📡 Relay

    Note over CLI,Relay: Step 1: Verify source agent

    CLI->>SrcR: GET /agt/handoff/status
    SrcR-->>CLI: handoff_available=true, registry_mode=global

    Note over CLI,Relay: Step 2: Initialize handoff session

    CLI->>SrcR: POST /agt/handoff/init {direction: local_to_aks}
    SrcR-->>CLI: {handoff_token, token_hash}

    Note over CLI,Relay: Step 3: Create state snapshot

    CLI->>SrcR: GET /agt/handoff/sub-agents
    SrcR-->>CLI: [{name, capabilities, amid}]
    CLI->>Sub: write .handoff-interrupt to each workspace
    Note right of Sub: 3s pause for agents<br/>to save checkpoints
    CLI->>Sub: docker exec tar workspace (each sub-agent)
    CLI->>SrcR: docker exec tar workspace (parent)
    CLI->>SrcR: POST /memory_stores/{store}:search_memories
    CLI->>SrcR: docker exec env (credential refs)
    CLI->>SrcR: POST /agt/handoff/snapshot {workspace_tar, sub_agent_snapshots[], credentials}
    SrcR-->>CLI: {blob, snapshot_size_bytes} (AES-256-GCM encrypted)

    Note over CLI,Relay: Step 4: Drain active work

    CLI->>SrcR: POST /agt/handoff/drain
    SrcR-->>CLI: drained ✓

    Note over CLI,Relay: Step 5: Provision target + restore

    CLI->>K8s: kubectl apply ClawSandbox CRD<br/>(handoff.mode=restore, predecessor AMID)
    Ctrl->>K8s: Reconcile → namespace, deployment, service, NetworkPolicy
    CLI->>K8s: kubectl create secret (credentials)
    CLI->>K8s: Wait for pod Ready (up to 120s)
    CLI->>TgtR: kubectl port-forward → POST /agt/handoff/restore {blob, shared_secret}
    Note right of TgtR: Decrypt blob → restore trust scores,<br/>audit entries, re-spawn sub-agents<br/>via POST /sandbox/spawn per snapshot

    TgtR-->>CLI: {trust_scores_count, sub_agent_results[]}
    TgtR->>K8s: Create sub-agent ClawSandbox CRDs
    Ctrl->>K8s: Reconcile → sub-agent pods

    Note over CLI,Relay: Plugin IIFE: async post-restore hydration

    Plugin->>Plugin: Parse chat_snapshot → replay to Foundry Conversation
    Plugin->>Plugin: Store handoff event in Foundry Memory
    Plugin->>Plugin: Extract workspace tar to /sandbox/
    Plugin->>Plugin: Write MEMORY.md + .handoff-state.json

    Note over CLI,Relay: Plugin: Sub-agent trust + resume loop

    Plugin->>Plugin: Collect original_amid from snapshots → staleAmids Set
    Plugin->>Plugin: Clear stale entries from nameToAmid cache

    loop For each sub-agent (reject stale AMIDs, 90s timeout)
        Plugin->>Reg: GET /registry/search?capability={name}
        Reg-->>Plugin: candidates (filter out staleAmids)
    end
    Note right of Plugin: Cache NEW AMIDs in nameToAmid

    loop Prekey gate (20 attempts × 3s)
        Plugin->>Relay: ping sub-agent (verify E2E session)
    end

    loop Workspace inject (3 attempts × 20s ack wait)
        Plugin->>Relay: mesh_send(workspace_inject, tar)
        Relay->>Sub: deliver workspace tar (E2E encrypted)
        Sub->>Sub: Extract tar + promote incoming/ + write HANDOFF_FILES.md
        Sub->>Relay: mesh_send(workspace_inject_ack, {file_count})
        Relay->>Plugin: deliver ack ✓
    end

    Plugin->>Relay: mesh_send(handoff:resume, {task_context, checkpoint})
    Relay->>Sub: deliver resume signal
    Sub->>Sub: Resume interrupted task (processTaskWithTools)

    Note over CLI,Relay: Step 6: Identity succession

    CLI->>Reg: POST /registry/succession (Ed25519 signed)
    Note right of Reg: Copy reputation, mark predecessor dormant

    Note over CLI,Relay: Step 7: Summary + local cleanup

    CLI->>SrcR: POST /agt/handoff/decommission
    Note right of SrcR: Parent goes dormant<br/>(Docker container preserved for reverse)
    CLI->>Sub: docker stop + rm sub-agent containers
```

### 12.3 Reverse Handoff: Cloud → Local (with Sub-Agents)

CLI-driven path (`azureclaw handoff <name> --to local`). 13 stepper steps.

```mermaid
sequenceDiagram
    autonumber
    participant CLI as 🖥️ CLI (handoff.ts)
    participant SrcR as 🛡️ Source Router<br/>(AKS)
    participant Sub as ☁️ AKS Sub-Agents
    participant K8s as ☸️ K8s API
    participant Docker as 🐳 Docker
    participant TgtR as 🛡️ Target Router<br/>(Docker)
    participant Plugin as 🔌 Target Plugin<br/>(Docker)
    participant Reg as 📋 Registry
    participant Relay as 📡 Relay

    Note over CLI,Relay: Step 1: Connect to AKS

    CLI->>K8s: kubectl port-forward svc/{name} 18445:8443
    CLI->>SrcR: GET /readyz (poll until healthy)

    Note over CLI,Relay: Step 2: Verify source agent

    CLI->>SrcR: GET /agt/handoff/status (via port-forward)
    SrcR-->>CLI: handoff_available=true

    Note over CLI,Relay: Step 3: Initialize handoff

    CLI->>SrcR: POST /agt/handoff/init {direction: aks_to_local}
    SrcR-->>CLI: {handoff_token, token_hash}

    Note over CLI,Relay: Step 4: Create state snapshot

    CLI->>SrcR: GET /agt/handoff/sub-agents
    SrcR-->>CLI: [{name, capabilities, amid}]
    CLI->>Sub: kubectl exec: write .handoff-interrupt
    Note right of Sub: 3s pause
    CLI->>Sub: kubectl exec: tar workspace (each sub-agent)
    CLI->>SrcR: kubectl exec: tar workspace (parent)
    CLI->>SrcR: POST /memory_stores/{store}:search_memories
    CLI->>SrcR: POST /agt/handoff/snapshot {workspace_tar, sub_agent_snapshots[]}
    SrcR-->>CLI: {blob, snapshot_size_bytes}

    Note over CLI,Relay: Step 5: Drain

    CLI->>SrcR: POST /agt/handoff/drain

    Note over CLI,Relay: Steps 6-9: Wake local + restore

    CLI->>Docker: docker start (wake dormant parent)
    CLI->>K8s: kubectl get secret → decode credentials
    CLI->>Docker: docker exec: write credentials to env file
    CLI->>TgtR: POST /agt/handoff/init {direction: aks_to_local}
    TgtR-->>CLI: {localHandoffToken}
    CLI->>TgtR: docker exec curl POST /agt/handoff/restore {blob, shared_secret}
    Note right of TgtR: Decrypt blob → restore state,<br/>re-spawn sub-agents as Docker containers
    TgtR-->>CLI: {trust_scores_count, sub_agent_results[]}

    Note over CLI,Relay: Plugin IIFE: async post-restore (same as §12.2)

    Plugin->>Plugin: Chat replay + memory + workspace extraction
    Plugin->>Plugin: Sub-agent trust+resume loop<br/>(stale AMID filter → prekey gate →<br/>workspace inject → resume)

    Note over CLI,Relay: Step 10: Identity succession

    CLI->>Reg: POST /registry/succession (Ed25519 signed)

    Note over CLI,Relay: Steps 11-13: Cloud teardown

    CLI->>SrcR: POST /agt/handoff/decommission
    CLI->>K8s: kubectl delete clawsandbox {parent}
    CLI->>K8s: kubectl delete clawsandbox {sub-agent-1}
    CLI->>K8s: kubectl delete clawsandbox {sub-agent-2}
    Note right of K8s: Controller cascades: delete<br/>namespace, deploy, svc, NetworkPolicy
    CLI->>CLI: aksPortForwardStop()
```

### 12.4 Stale AMID Cache Poisoning — Problem & Fix

After handoff, the predecessor's sub-agents leave stale AMIDs in the registry (5-minute heartbeat timeout). The successor's trust+resume loop could cache these dead AMIDs, causing all mesh messages to silently drop.

```mermaid
sequenceDiagram
    participant Old as Old Sub-Agent<br/>(AMID: 3FFu...)
    participant Reg as 📋 Registry
    participant New as New Sub-Agent<br/>(AMID: 4SKY...)
    participant Parent as Successor Parent

    Note over Old,Parent: T+0: Docker containers destroyed

    Old->>Reg: Last heartbeat at T-30s
    Note right of Reg: AMID 3FFu... still "online"<br/>(heartbeat timeout = 5 min)

    Parent->>Reg: search("researcher")
    Reg-->>Parent: 3FFu... (STALE ❌)
    Note right of Parent: original_amid filter rejects!<br/>Keeps searching...

    Note over Old,Parent: T+27s: New AKS sub-agent registers

    New->>Reg: Register AMID 4SKY...
    Note right of Reg: Ghost cleanup deletes 3FFu...<br/>4SKY... is now "online"

    Parent->>Reg: search("researcher")
    Reg-->>Parent: 4SKY... (NEW ✅)
    Parent->>Parent: Cache 4SKY... in nameToAmid
```

**Three-layer fix:**
1. **Stale AMID rejection** — `original_amid` from snapshots used to filter registry results by identity, not time
2. **Prekey readiness gate** — 20 attempts × 3s to verify E2E session before workspace_inject
3. **Workspace inject retry** — 3 attempts with 20s ack wait, catches send errors

### 12.5 Workspace Injection Detail

```mermaid
%%{init: {'theme': 'default'}}%%
graph TB
    subgraph sender["Parent — sender side"]
        S1["Collect workspace tar<br/>from snapshot"] --> S2["mesh_send(workspace_inject,<br/>base64 tar)"]
        S2 --> S3{"Ack received<br/>within 20s?"}
        S3 -->|No| S4["Retry (max 3)"]
        S4 --> S2
        S3 -->|Yes| S5["Send handoff:resume<br/>with task_context"]
    end

    subgraph receiver["Sub-Agent — receiver side"]
        R1["Receive workspace_inject"] --> R2["Validate tar entries<br/>(no path traversal)"]
        R2 --> R3["Extract to /sandbox/<br/>(--no-overwrite-dir)"]
        R3 --> R4["Promote incoming/ files<br/>to workspace root"]
        R4 --> R5["Write HANDOFF_FILES.md<br/>(file manifest)"]
        R5 --> R6["Send workspace_inject_ack<br/>{success, file_count}"]
    end

    S2 -.->|E2E encrypted<br/>via relay| R1
    R6 -.->|E2E encrypted<br/>via relay| S3
```
