<!--
Copyright (c) Microsoft Corporation.
Licensed under the MIT License.
-->

# kars — showcase source of truth

Every claim below is ground-truthed against repo HEAD with file:line citations. Update this file; regenerate the deliverables.

---

## 1 · The race

Every major vendor is shipping an agent runtime: Bedrock Agents, Vertex Agents, OpenAI Assistants, LangChain / LangGraph Cloud, CrewAI, AutoGPT.

They all ship the agent loop, tools, and some memory.

None ship all of these together:
- **Kubernetes-native** deployment, on your cluster, with your RBAC.
- **E2E encrypted inter-agent mesh** — Signal Protocol, no central broker sees plaintext.
- **Multiple agent frameworks in one cluster** — pick per agent.
- **Governance enforced in the data plane** — every model call + tool call + outbound HTTPS through a per-pod proxy.

That's the gap.

---

## 2 · What kars is

> Secure, multi-runtime AI agent runtime on Azure Kubernetes Service. E2E encrypted inter-agent mesh. Governance enforced in the per-sandbox data plane. Built on the Microsoft Agent Governance Toolkit.

---

## 3 · Four pillars

### 3.1 · Sandbox

One Kubernetes namespace + one pod per agent.
- `kars-strict` seccomp profile (default `SCMP_ACT_ERRNO`; denies `mount`, `ptrace`, `unshare`, `setns`, `bpf`, `kexec_*`, `init_module`).
  Source: `deploy/helm/kars/files/kars-strict.json`
- Init container plants iptables: agent UID 1000 → loopback + DNS only; ports 80/443 → REDIRECT to router :8444.
  Source: `controller/src/reconciler/mod.rs:1916-1958`
- `readOnlyRootFilesystem`, `runAsNonRoot`, `drop ALL caps` on every container.
  Source: `controller/src/reconciler/mod.rs:1878-1888`
- Confidential isolation maps to `runtimeClassName: kata-vm-isolation` + node pool `sandbox-kata`.
  Source: `controller/src/reconciler/mod.rs:85-89`
- 7 ValidatingAdmissionPolicies prevent posture downgrades (no exec, no public exposure, no privileged, no null providers, …).
  Source: `deploy/helm/kars/templates/admission-*.yaml`

### 3.2 · AGT mesh

E2E encrypted inter-agent messaging using the Signal Protocol.
- **Identity**: Ed25519 signing + X25519 key exchange, derived from one 32-byte seed.
  Source: `mesh-plugin/src/identity.ts:163-237`
- **DID**: `did:mesh:<sha256(public_key)[:32]>`.
  Source: `mesh-plugin/src/did.ts:31-55`
- **X3DH**: signed prekeys + one-time prekeys uploaded to registry; session derived per-peer.
  Source: `runtimes/agt-mesh-python/src/kars_agt_mesh/client.py:257-288, 423-444`
- **KNOCK protocol**: session establishment frame carries the initiator's ChannelEstablishment + intent; receiver's policy gates accept.
  Source: `runtimes/agt-mesh-python/src/kars_agt_mesh/client.py:494-548`
- **Cross-runtime wire-format proof**: 8 regression tests assert Python AGT emits byte-identical frames to the TypeScript SDK.
  Source: `runtimes/agt-mesh-python/tests/test_wire_format.py`
- Relay + registry run in-cluster (`agentmesh` namespace); router proxies `/agt/relay` (WS) + `/agt/registry/*` (HTTP).
  Source: `inference-router/src/routes/mesh.rs:24-44`

### 3.3 · Governance as data plane

A per-sandbox inference-router sidecar is the only network egress path for the agent.
- **InferencePolicy** — primary + ordered fallback chain; daily/monthly/per-request token budgets per sandbox.
  Source: `inference-router/src/failover.rs:51-95` + `inference-router/src/budget.rs:203-257`
- **ToolPolicy** — every chat-completions call rewrites the `tools` array to drop denied tools; AGT profile evaluated per `tool.invoke:<name>`.
  Source: `inference-router/src/routes/chat_completions.rs:1004-1097`
- **Content safety** — Foundry guardrails + Prompt Shields enforcement (jailbreak detection, content filters).
  Source: `inference-router/src/safety.rs:497-565`
- **Egress allowlist + blocklist** on the transparent forward proxy at :8444; learn-mode for dev, strict-mode for prod, signed bundle source of truth.
  Source: `inference-router/src/blocklist.rs:313-366` + `inference-router/src/forward_proxy.rs:4-11`
- **Audit** — append-only JSONL with `prev_hash` + `hash` chain per row, date-rotated.
  Source: `inference-router/src/audit_jsonl.rs:60-70` (note: hash-chained, not cryptographically signed)
- **Copilot** path — IDE-JWT exchange cached + refreshed proactively (respects `expires_at`); auto-fallback chain on 503.
  Source: `inference-router/src/copilot_auth.rs:146-229`

### 3.4 · Runtime contract v1

Minimal env + endpoint contract any agent framework can adopt.
- Controller injects `KARS_MODEL`, `KARS_RUNTIME_KIND`, `KARS_RUNTIME_CONTRACT_VERSION=v1`.
  Source: `controller/src/reconciler/mod.rs:1330-1347`
- Router lives at `http://127.0.0.1:8443` (loopback only — iptables enforced).
  Source: `docs/runtimes/CONTRACT.md:132-155`
- BYO contract: image must declare `org.kars.runtime.contract=v1` label.
  Source: `controller/src/reconciler/byo_contract.rs:16-21`
- 8 wired runtimes today, +1 placeholder.
  Source: `cli/src/runtime.ts:54-64` + `controller/src/reconciler/runtime.rs:345-439`

---

## 4 · Architecture (diagram 02)

Three layers, color-coded:
- **Controller** in `kars-system` — reconciles 11 CRDs.
- **AGT Mesh** in `agentmesh` namespace — relay + registry; E2E encrypted, never sees plaintext.
- **Sandbox** in `kars-<agent>` (one per agent) — init `egress-guard`, container `agent`, sidecar `inference-router`.

The agent's only egress is `127.0.0.1:8443` → inference-router → outside world.

---

## 5 · Sandbox anatomy (diagram 03)

- **init: egress-guard** (UID 0, runs once): iptables → agent UID 1000 reaches only loopback + DNS.
- **container: agent** (UID 1000): the runtime. Read-only root, drop ALL caps.
- **sidecar: inference-router** (UID 1001): governance choke point + only path out.

---

## 6 · AGT mesh flow (diagram 04)

Three steps:
1. Agent A → registry: discover B (returns prekey bundle)
2. Agent A → relay: KNOCK + encrypted payload (X3DH session key, then Double Ratchet)
3. Relay → Agent B: deliver opaque bytes; B's policy gates KNOCK accept

Relay never holds plaintext. Forward secrecy via Double Ratchet rotation.

---

## 7 · Governance layers (diagram 05)

Four independent layers:
1. iptables egress-guard (init container)
2. NetworkPolicy (cluster-level)
3. Inference router (InferencePolicy, ToolPolicy, Content Safety, budgets)
4. AGT policy hook (per tool call)

Every decision lands in the hash-chained audit JSONL.

---

## 8 · Six deployment blueprints (diagram 06)

One chart. Trust boundary moves with deployment shape.

| # | Blueprint | Where | Status |
|---|---|---|---|
| 01 | Developer inner loop | laptop · docker | documented |
| 02 | Local Kubernetes dev | laptop · kind | documented |
| 03 | Enterprise self-hosted | AKS · single tenant | shipped |
| 04 | Managed public offload | AKS · multi-tenant + Kata + SEV-SNP | shipped runtime |
| 05 | Cross-org federation | two AKS · mesh + A2A | shipped (real code, ongoing UX work) |
| 06 | Sovereign / air-gapped | isolated AKS · no public egress | composable from primitives; one-command roadmap |

Source: `docs/blueprints/00-index.md`, `docs/blueprints/01..06-*.md`

---

## 9 · Multi-runtime (diagram 07)

8 wired runtimes today, BYO for the rest:

| Runtime | Language | Image | Adapter |
|---|---|---|---|
| OpenClaw | TypeScript | `sandbox-images/openclaw/` | `runtimes/openclaw/` (24 declared plugin tools) |
| Hermes | Python 3.11+ | `sandbox-images/hermes/` | `runtimes/hermes/` (15 plugin tools, multi-channel) |
| Anthropic | Python | `sandbox-images/anthropic/` | `runtimes/anthropic/` |
| MAF Python | Python | `sandbox-images/maf-python/` | `runtimes/maf-python/` (.NET deferred) |
| LangGraph py | Python | `sandbox-images/langgraph/` | `runtimes/langgraph/` |
| LangGraph ts | TypeScript | `sandbox-images/langgraph-ts/` | `runtimes/langgraph-ts/` |
| Pydantic AI | Python | `sandbox-images/pydantic-ai/` | `runtimes/pydantic-ai/` |
| OpenAI Agents | Python | `sandbox-images/openai-agents/` | `runtimes/openai-agents/` |
| BYO | any | your image | `org.kars.runtime.contract=v1` label |

Same control plane. Same policies. Pick per agent. Mix per cluster.

Source: `cli/src/runtime.ts:54-64` (single source of truth list)

---

## 10 · Built on AGT

kars sits on top of the open-source Microsoft Agent Governance Toolkit.

**From AGT we consume:**
- Signal protocol (X3DH, Double Ratchet, SecureChannel) — TypeScript + Python
- Mesh relay + registry servers (Python FastAPI)
- Cedar-policy governance evaluator

**To AGT we contribute back (in flight):**
- Proof-of-possession on `/ws` connect frames (upstream PR #2772)
- X3DH KDF spec-compliance (separate PR pending)
- Multiple Python MeshClient compatibility fixes (already landed)

Pin: `vendor/agt/pin.json` — branch `kars-sdk-pop-signing` @ `3322175d`

**Where AGT ends and kars begins:**
- AGT = the protocol + libraries.
- kars = the Kubernetes-native production runtime + governance data plane.

---

## 11 · What's next

*(content fine-tuned with the user before slide commit)*

- **Hermes Act 2** — full Python MeshClient parity with TypeScript SDK (Act 1 just shipped)
- **kars-sre** — in-cluster SRE agent for auto-diagnosis + governance-gated auto-remediation
  See: `docs/blueprints/07-kars-sre-proposal.md`
- **Sovereign / air-gapped graduation** — one-command bundle for Blueprint 06
- **Cross-org federation UX** — `kars pair` flow polish for Blueprint 05
- **Confidential-computing attestation** — Kata + SEV-SNP shipped, attestation-gated workloads TBD

---

## 12 · CTAs

```bash
git clone https://github.com/Azure/kars
cd kars/cli && npm ci && npm run build && npm link
kars dev
```

Contribute: `docs/CONTRIBUTING.md` · runtime contract: `docs/runtimes/CONTRACT.md`

---

## Deliverable mapping

| Deliverable | Sections | Diagrams | Notes |
|---|---|---|---|
| Pitch deck (15 slides) | 1, 2, 3 (4 sub-slides), 4, 5, 6, 7, 8, 9, 10, 11, 12 | 02, 03, 04, 05, 06, 07 | Hero shot per slide; max 30 words per slide |
| Conference keynote (30 slides) | All 12 + demo slots | All diagrams + live `kars operator` screenshots | Architectural deep-dives + live demos |
| Web showcase (HTML) | All 12 | Clickable diagrams, expandable blueprints, animated mesh flow | Self-contained, Tailwind + Clawpilot theme |

---

## Visual conventions (locked, do not drift)

- One hero idea per slide. Max 30 words.
- Diagrams: ≤7 named boxes, ≤3 arrow concepts, 2-3 colors max.
- Color semantics: blue=controller, purple=mesh, green=runtime/sandbox/agent, teal=router, orange=egress-guard, gray=neutral.
- Text on slides: short noun phrases, not sentences. Sentences go in speaker notes.
- Citations live in this outline, NOT on slides.
