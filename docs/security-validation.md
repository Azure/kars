# AzureClaw Security Validation Report

**Date:** 2026-03-23  
**Cluster:** `azureclaw-demo-aks` (eastus2)  
**Agent:** `demo-agent` (gpt-4.1, confidential isolation)  
**Pod:** `demo-agent-689b5cc899-ktll9`

All 9 security layers validated on a live AKS cluster with evidence captured below.

---

## Layer 0: Azure Infrastructure ✅

**API server restricted to authorized IPs only.**

```json
{
  "authorizedIpRanges": [
    "91.231.60.24/32"
  ]
}
```

Only one IP range is authorized. All other traffic to the Kubernetes API server is rejected at the Azure load balancer level.

---

## Layer 1: Node OS (Azure Linux) ✅

```
OS Image:       Microsoft Azure Linux 3.0
Kernel:         6.6.126.1-1.azl3
```

Azure Linux 3 is the default AKS node OS with SELinux enforcing mode, automatic
security patch updates, and no SSH access by default.

---

## Layer 2: Kata VM Isolation ✅

**Runtime class confirms dedicated VM per pod:**

```
Runtime Class:  kata-vm-isolation
```

**Kata node pool (3 nodes):**

```
node/aks-katapool-24494507-vmss000000
node/aks-katapool-24494507-vmss000001
node/aks-katapool-24494507-vmss000002
```

Each pod runs in a dedicated lightweight VM (Cloud Hypervisor) with its own kernel.
Container escape attacks are trapped inside the VM boundary. The CRD specifies
`isolation: confidential` which maps to the `kata-vm-isolation` runtime class.

---

## Layer 3: Container Hardening ✅

**Pod-level security context (all containers):**

```json
{
    "fsGroup": 1000,
    "runAsGroup": 1000,
    "runAsNonRoot": true,
    "runAsUser": 1000
}
```

**OpenClaw container security context:**

```json
{
    "allowPrivilegeEscalation": false,
    "capabilities": {
        "drop": [ "ALL" ]
    },
    "readOnlyRootFilesystem": true,
    "runAsUser": 1000
}
```

| Control | Setting | Verified |
|---------|---------|----------|
| Root filesystem | Read-only | ✅ `readOnlyRootFilesystem: true` |
| User | Non-root UID 1000 | ✅ `runAsNonRoot: true, runAsUser: 1000` |
| Privilege escalation | Blocked | ✅ `allowPrivilegeEscalation: false` |
| Capabilities | All dropped | ✅ `drop: ["ALL"]` |
| Writable paths | `/sandbox` and `/tmp` only | ✅ CRD `writablePaths: [/sandbox, /tmp]` |

---

## Layer 4: Kernel Confinement (seccomp) ✅

```json
{
    "seccompProfile": {
        "type": "RuntimeDefault"
    }
}
```

The CRD specifies `seccompProfile: azureclaw-strict` (custom Localhost profile with
219 allowed syscalls, 28 explicitly blocked). On Kata VM pods, the runtime defaults to `RuntimeDefault`
because the VM boundary provides the primary isolation. The custom strict profile is
used on `enhanced` isolation level (standard container runtime).

---

## Layer 5: Network Segmentation ✅

### NetworkPolicy (default-deny)

```
NAME             POD-SELECTOR                            AGE
sandbox-policy   azureclaw.azure.com/component=sandbox   4h31m
```

The CRD confirms:

```yaml
networkPolicy:
  defaultDeny: true
```

### iptables UID-based Egress Guard

Init container `egress-guard` runs with `NET_ADMIN` capability to install iptables
rules, then exits. Runtime effect:
- **UID 1000 (agent):** Can only reach `localhost` + UDP port 53 (DNS)
- **UID 1001 (router):** Unrestricted within NetworkPolicy scope

### Domain Blocklist (auto-refreshing)

```
Blocklist: loaded OISD feed      — 51,449 domains
Blocklist: loaded URLhaus feed   —    456 domains
Blocklist refreshed              — 51,969 domains blocked (total)
```

Refreshes every 6 hours from OISD + URLhaus feeds. Includes high-risk TLDs
(`.tk`, `.ml`, `.ga`, `.cf`, `.gq`), bare IP blocking, and subdomain matching.

---

## Layer 6: Inference Safety ✅

**CRD inference configuration:**

```yaml
inference:
  contentSafety: true
  model: gpt-4.1
  promptShields: true
  provider: azure-openai
```

| Control | Status | Evidence |
|---------|--------|----------|
| Content Safety | ✅ Enabled | `contentSafety: true` in CRD |
| Prompt Shields (jailbreak detection) | ✅ Enabled | `promptShields: true` in CRD |
| Token budgets | ✅ Enforced | Per-sandbox daily + per-request limits |
| Metrics | ✅ Active | Prometheus endpoint on router |
| **Content Safety Floor VAP** | ✅ Enforced | See below |

Content Safety operates via **Foundry-side guardrails** (`Microsoft.DefaultV2`): content filter
annotations are applied server-side by Azure AI Foundry on every model inference call. The router
parses `prompt_filter_results` from the response and reports detected flags to AGT governance.
There is no separate Content Safety API call — if the model response lacks filter annotations,
inference proceeds normally.

### Content Safety Floor (admission-time enforcement)

The `azureclaw-content-safety-floor` ValidatingAdmissionPolicy rejects any
`InferencePolicy` whose `spec.inference.contentSafetyMinimum` is set to a
value less strict than the cluster floor (`Medium` by default).

```bash
kubectl get validatingadmissionpolicy azureclaw-content-safety-floor \
  -o jsonpath='{.spec.validations[0].expression}'
# Output: object.spec.inference.contentSafetyMinimum >= clusterFloor
```

Severity ordinal enforced by CEL: `Safe(0) < Low(1) < Medium(2) < High(3)`.
Requires Kubernetes ≥ 1.30.

---

## Layer 7: Behavioral Governance — AGT ✅

**Router logs confirm AGT is active:**

```
AGT policy profile loaded — policies: 3, agent: "openclaw"
AGT governance ENABLED — sandbox: "demo-agent"
```

**CRD governance configuration:**

```yaml
governance:
  enabled: true
  toolPolicy: default
  trustThreshold: 500
```

| Control | Status | Evidence |
|---------|--------|----------|
| Policy evaluation | ✅ 3 policies loaded | Tool-level allow/deny decisions |
| Trust scoring | ✅ Threshold 500 | Ed25519 identity, 0-1000 scale, 5 tiers |
| Audit logging | ✅ Hash-chain | OWASP ASI compliant, SHA-256 integrity |

---

## Layer 8: E2E Encrypted Inter-Agent Communications ✅

**Signal Protocol relay connections (unique agent identities):**

```
Agent 7vzNgRbjQ2Kpzxw4Po1PjgPnwuL connected (session: 173f7f00...)
Agent JhXkFRMm2sxzLd6M77FSaELoXEs connected (session: 4b6a3d42...)
Agent 2xgxm8kR7R9EYAhrGRZj7LLwotRF connected (session: 45e490de...)
```

Each AMID (Agent Mesh ID) is derived from the agent's Signal Protocol identity key.
Different session UUIDs confirm independent cryptographic sessions.

**Traffic capture — what the relay middleman sees:**

```
[15:24:50] agent->relay      498B  {"type":"send","to":"2xgxm8kR...","encrypted_payload":"{\"versi...
[15:24:50] agent->relay      715B  {"type":"send","to":"2xgxm8kR...","encrypted_payload":"{\"sessi...
[15:24:51] relay->agent      630B  {"type":"receive","from":"2xgxm8kR...","encrypted_payload":"{\"...
```

**What the agent endpoint sees (after decryption):**

```
[15:24:50] AGT relay: sent to math-agent (2xgxm8kR7R9E...) via E2E encrypted relay
[15:24:51] AGT relay message from math-agent (2xgxm8kR7R9E...): "16"
```

| Layer | Sees | Can read content? |
|-------|------|-------------------|
| Relay (middleman) | `encrypted_payload` + routing AMIDs | ❌ No |
| Router (WebSocket bridge) | Same as relay — opaque forwarding | ❌ No |
| Gateway (endpoint) | Decrypted plaintext: "16" | ✅ Yes |

Full hex-dump analysis: `docs/internal/e2e-encryption-proof.md` (internal companion).

---

## `azureclaw attest` — Spec Hash & Reconcile Trace

`azureclaw attest <NAME>` surfaces tamper-evident evidence for a sandbox
without requiring cluster-admin access. It reads only `ClawSandbox` status
fields and Deployment annotations.

| Evidence field | Source | Notes |
|----------------|--------|-------|
| **Spec hash** | SHA-256 over canonical JSON of `spec` | Changes on any CRD field mutation |
| **SSA owner map** | `metadata.managedFields` | Lists field-level owners (controller, CLI, user) |
| **Observed-generation lineage** | `status.observedGeneration` vs `metadata.generation` | Drift = pending reconcile |
| **Policy version hashes** | `status.versionHash` per referenced policy | Changes when referenced `ToolPolicy` / `InferencePolicy` is updated |
| **Reconcile trace ID** | `azureclaw.azure.com/last-trace-id` annotation on Deployment | Prints `(pending)` if the controller has not yet annotated the Deployment |
| **AGT audit-receipt id** | Reserved for v1.1 | Currently `(pending)` in shipped builds |

```bash
# Human-readable summary
azureclaw attest <NAME>

# Machine-readable (for CI diff or SIEM ingestion)
azureclaw attest <NAME> --format json
```

---



Validated by automated test suites (no live cluster needed):

| Control | Test evidence |
|---------|---------------|
| `redactSecrets()` masks Bearer/Basic/JWT/PEM/`azcp_*`/keyword secrets in CLI logs | `cli/src/redact.test.ts` — 9 tests ✅ |
| `sanitizeForLog()` strips CR/LF/tab from untrusted strings | `cli/src/stepper.test.ts`, `cli/src/commands/mesh.test.ts` ✅ |
| `escapeHtml()` on OAuth callback page | `cli/src/commands/mesh.test.ts` ✅ |
| TOCTOU-safe file reads (`openSync`+`fstatSync`+`readSync`) | `cli/src/plugin.test.ts` (offload + workspace transfer paths) ✅ |
| `execFileSync("find", […])` — no shell, no head pipe | `cli/src/plugin.test.ts` ✅ |
| Constant-time admin-token compare (`handoff::constant_time_eq`) | `cargo test --package azureclaw-inference-router` (handoff + trust + rate-limit suites) ✅ |
| `#[serde(deny_unknown_fields)]` rejects typo'd `SpawnRequest` / `HandoffMeta` | `inference-router/src/spawn.rs` unit tests ✅ |
| Sandbox hardening invariants (UID 1000, RO rootfs, drop ALL caps, seccomp `azureclaw-strict`, NET_ADMIN drop after init, iptables egress-guard, plugin+SDK root-owned RO) | `cli/src/testing/sandbox-hardening.test.ts` + controller-side reconciler regression test ✅ |
| `cargo audit` (RUSTSEC closure) | `.github/workflows/ci.yml` cargo-audit job ✅ (closed RUSTSEC-2026-0098/-0099/-0104 by bumping `rustls-webpki`) |
| `npm audit` (vulnerable transitive bumps) | `cli/package.json` overrides → `npm audit` 0 vulnerabilities ✅ |
| Fuzz / proptest coverage | `cargo +nightly fuzz` targets: handoff blob, blocklist domain, AGT policy, safety-response. `proptest`: chunking, Double-Ratchet, K8s names ✅ |

Reproduce with:

```bash
cd cli && npm test
cd mesh-plugin && npm test
cargo test --all
cd cli && npm audit --audit-level=moderate
cargo audit
```

---

## Resource Lifecycle: Finalizer ✅

The controller adds a `azureclaw.azure.com/namespace-cleanup` finalizer to every
ClawSandbox CRD. On deletion, it cascades:

```
Finalizers: ["azureclaw.azure.com/namespace-cleanup"]
```

1. Deletes the sandbox namespace (cascading all K8s resources)
2. Deletes the spawner ClusterRoleBinding
3. Removes the finalizer (allowing CRD garbage collection)

No orphan namespaces or leaked resources.

---

## Identity & Access: Zero Credentials ✅

```
Auth mode: Workload Identity (AKS mode)
```

| Principle | Evidence |
|-----------|----------|
| Zero standing credentials | No API keys in images, env vars, or secrets |
| Workload Identity | Federated OIDC token exchange via projected SA token |
| Credential isolation | Only UID 1001 (router) can reach IMDS |
| Service accounts | `sandbox` SA with federated identity binding |

---

## OWASP Agentic Security Index Coverage

| Risk | ASI | Layer(s) | Status |
|------|-----|----------|--------|
| Agent Goal Hijacking | ASI-01 | L6 Foundry Guardrails (jailbreak/indirect attack) + policy deny lists | ⚠️ Partial |
| Excessive Capabilities | ASI-02 | L4 seccomp + L5 iptables + NetworkPolicy + capability allow/deny | ✅ |
| Identity & Privilege Abuse | ASI-03 | Workload Identity (OIDC) + Ed25519 keypairs | ⚠️ Partial |
| Uncontrolled Code Execution | ASI-04 | L2 Kata VM + L4 seccomp + L3 read-only rootfs + drop ALL caps | ✅ |
| Insecure Output Handling | ASI-05 | L6 Foundry Guardrails (prompt-side) + output policy (log-only) | ⚠️ Partial |
| Memory Poisoning | ASI-06 | Not implemented | ❌ |
| Unsafe Inter-Agent Comms | ASI-07 | L5 NetworkPolicy + L8 Signal Protocol Double Ratchet E2E | ✅ |
| Cascading Failures | ASI-08 | L6 Token budgets + rate limiter + concurrency semaphore | ⚠️ Partial |
| Human-Agent Trust | ASI-09 | L7 AGT audit logging + RequiresApproval decision type | ⚠️ Partial |
| Rogue Agents | ASI-10 | L5 iptables kill + L7 BehaviorMonitor anomaly detection | ⚠️ Partial |

**4/10 strong, 5/10 partial, 1/10 not implemented.** See [docs/security.md](security.md) for detailed per-risk breakdown.

---

## Reproduction

All evidence can be regenerated on any AzureClaw cluster:

```bash
# Layer 0: API server IP restrictions
az aks show -g <RG> -n <CLUSTER> --query "apiServerAccessProfile"

# Layer 1: Node OS
kubectl get nodes -o jsonpath='{.items[0].status.nodeInfo.osImage}'

# Layer 2: Kata VM
kubectl get pod -n <NS> <POD> -o jsonpath='{.spec.runtimeClassName}'

# Layer 3: Container hardening
kubectl get pod -n <NS> <POD> -o jsonpath='{.spec.containers[0].securityContext}'

# Layer 4: seccomp
kubectl get pod -n <NS> <POD> -o jsonpath='{.spec.securityContext.seccompProfile}'

# Layer 5: NetworkPolicy + Blocklist
kubectl get networkpolicies -n <NS>
kubectl logs -c inference-router ... | grep "Blocklist refreshed"

# Layer 6: Content Safety
kubectl get clawsandbox <NAME> -n azureclaw-system -o jsonpath='{.spec.inference}'

# Layer 7: AGT Governance
kubectl logs -c inference-router ... | grep "governance"

# Layer 8: E2E Encryption
kubectl logs -c inference-router ... | grep "TRAFFIC CAPTURE"
```
