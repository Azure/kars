# Agentic AI state of the art — kars capability map

**Date:** 2026-06-15
**Sources verified:** OWASP Agentic Top 10 (2026), NIST AI RMF Agentic Profile (CSA, 2026-03-27, draft v1), AAGATE reference architecture (CSA, 2025-12), MCPSHIELD formal framework (arXiv 2604.05969, 2026-04), Open Challenges in Multi-Agent Security (arXiv 2505.02077), CSA Antigravity Sandbox Escape research note (2026-04-22), industry security guidance (OWASP LLM Top 10).

**Purpose:** Honestly map kars against the SOTA literature and threat taxonomies. Identify where kars is best-in-class, where it is competitive, and where there are real gaps. Drive the next round of strategic work items.

---

## Executive summary

| Status | Count | What it means |
|---|---|---|
| **Best in class** | 3 of 10 | kars exceeds SOTA reference architectures (ASI-05 inter-agent communication, ASI-07 code execution containment, identity surface) |
| **Competitive** | 4 of 10 | kars at or near SOTA, with documented gaps in the parity plan (ASI-02 tool misuse, ASI-03 identity, ASI-06 supply chain, ASI-04 memory) |
| **Behind SOTA** | 3 of 10 | kars has partial coverage; real gaps vs published reference architectures (ASI-08 cascading failures, ASI-09 human-trust, ASI-10 behavioral drift) |
| **Not yet covered** | 4 cross-cutting | Autonomy tier classification (NIST), behavioral drift / cognitive degradation monitoring (AAGATE QSAF), fleet-wide millisecond kill-switch (AAGATE GOA), continuous compliance evaluation (AAGATE ComplianceAgent) |

**The headline:** kars is genuinely state-of-the-art on the *trust-boundary + inter-agent communication* axes (the moat), competitive on most surfaces, and **structurally behind on the runtime-governance / behavioral-observability axes** that the NIST Agentic Profile and AAGATE define. Closing the runtime-governance gap is what makes kars not just "more secure than the alternatives" but "the reference implementation for agentic AI on Kubernetes".

---

## The reference materials (what "SOTA" actually means in 2026)

### OWASP Top 10 for Agentic Applications (ASI series, 2026)

The authoritative threat taxonomy for agentic AI deployments. Ten categories:

| | Category | One-line definition |
|---|---|---|
| ASI-01 | Agent Goal Hijack | Attacker steers the agent's objective via prompt injection, poisoned inputs, context tampering |
| ASI-02 | Tool Misuse and Exploitation | Legitimate tools abused or subverted under agent's privileges |
| ASI-03 | Identity & Privilege Abuse | Misuse of agent identities (tokens, SAs, federated creds, Agent IDs) |
| ASI-04 | Memory & Context Poisoning | Persistent memory stores polluted to influence future agent behavior |
| ASI-05 | Insecure Inter-Agent Communication | Plaintext-broker A2A, missing authn between agents, missing forward secrecy |
| ASI-06 | Supply Chain Exposure | Model, tool, plugin, MCP server, container image supply chain compromise |
| ASI-07 | Unexpected Code Execution | Sandbox escape, kernel exploit, capability escalation from agent process |
| ASI-08 | Cascading Failures | One failing agent / sub-agent propagates through delegation chains |
| ASI-09 | Human-Agent Trust Exploitation | Agent emits trustworthy-looking output that misleads operator approvals |
| ASI-10 | Rogue Agents & Behavioral Drift | Long-term agent behavior departs from baseline; insider-threat by the agent itself |

### NIST AI RMF Agentic Profile (CSA, March 2026, draft v1)

Augments NIST AI RMF 1.0 (Jan 2023) and the Generative AI Profile (NIST AI 600-1, 2024) with four agentic-specific extension categories:

| RMF function | Agentic extension |
|---|---|
| **GOVERN** | Formal **autonomy tier classification** + Agent Owner roles + Agentic AI Committees + dedicated reporting lines |
| **MAP** | Systematic **tool-use risk modeling** + action-consequence mapping + propagation modeling across agent chains |
| **MEASURE** | Real-time behavioral metrics + autonomy calibration + **delegation chain monitoring** for runaway processes |
| **MANAGE** | Incident response for **agent compromise**, **behavioral drift correction**, **principled decommissioning** |

The "agentic fitness gap" the paper identifies: vanilla RMF treats "a system that recommends" and "a system that autonomously executes multi-day workflows with external effects" identically. Different autonomy levels need different oversight obligations, and the RMF 1.0 provides no taxonomy.

### AAGATE: Agentic AI Governance Assurance & Trust Engine (CSA, Dec 2025, [arXiv 2510.25863](https://arxiv.org/abs/2510.25863))

The CSA's K8s-native reference architecture for runtime governance of agentic AI. Eight-component overlay:

1. **Governing-Orchestrator Agent (GOA)** — central "brain"; ingests security telemetry; SEI SSVC classification; **millisecond kill-switch** authority across the fleet
2. **ComplianceAgent** — continuous evaluation against OWASP AIVSS + OPA/Rego policies (not just compile-time)
3. **Behavioral Analytics** — observability for anomaly detection + behavioral drift detection
4. **Zero-Trust Service Mesh** — strong network segmentation + identity (assumed mTLS-style)
5. **Decentralized Accountability** — logging, auditing, optional on-chain recordkeeping
6. **LPCI (Logic-Layer Prompt Control Injection) defenses** — protect against prompt injection at the reasoning + tool-use layer
7. **QSAF (Cognitive Degradation Monitoring)** — detects reasoning drift / recursive confusion
8. **DIRF (Digital Identity Rights Framework)** — controls over digital identities, agent likeness replication

Plus seven continuous control loops including the kill-switch and automated adversarial red-teaming.

Aligned to: NIST AI RMF, CSA AI Controls Matrix (243 controls, 18 domains, July 2025), CSA MAESTRO threat modeling, OWASP AIVSS scoring, SEI SSVC.

### MCPSHIELD: Formal security framework for MCP-based agents ([arXiv 2604.05969](https://arxiv.org/abs/2604.05969), April 2026)

**Key claim verified by the paper:** of 12 existing MCP defense strategies surveyed, **none cover more than 34% of identified threats** individually. MCPSHIELD's defense-in-depth combination achieves theoretical coverage of 91%.

Threat taxonomy: 7 categories × 23 distinct attack vectors across 4 surfaces.

Recommended defense-in-depth stack:
- Capability-based access control
- Cryptographic attestation of tools + data + agents
- Information flow tracking
- Runtime policy enforcement
- Formal verification of agent-tool interactions

**Takeaway:** any single defensive technique (prompt filter, allow-list, sandbox, attestation) is insufficient alone. Multi-layered architectures are required.

### Open Challenges in Multi-Agent Security ([arXiv 2505.02077](https://arxiv.org/abs/2505.02077))

New threat classes specific to multi-agent systems:
- **Secret collusion** between agents that individually look policy-compliant
- **Coordinated swarm attacks** that fan out across multiple agents
- **Fast-propagating disinformation** through agent-to-agent message chains
- **Privacy breaches via inter-agent aggregation** (each agent has a non-sensitive view; combined view is)
- **Stealth attacks** enabled by attack-surface dispersion across the fleet

Calls for a new field — "multi-agent security" — bridging AI safety and traditional cybersecurity.

### CSA Antigravity Sandbox Escape (April 2026)

Documents a real-world prompt-injection-to-sandbox-escape in agentic IDEs. Demonstrates that LLM-driven tool use can chain together (a) prompt injection from a poisoned input, (b) native tool abuse, (c) sandbox escape. Reinforces the "agent code is adversarial" claim and underscores that container hardening alone is insufficient — egress confinement matters because that's the only way the attacker can exfiltrate after escape.

---

## Mapping kars against the OWASP Agentic Top 10 (ASI-01 to ASI-10)

| | Threat | Kars coverage | Status | Gap |
|---|---|---|---|---|
| **ASI-01** | Agent Goal Hijack | Prompt Shields content-safety on every chat completion; KNOCK gate on inbound mesh; AGT governance hook on tools; per-call OTel for forensics | **Competitive** | Single-layer guardrail today (Prompt Shields only). Parity plan items #7-#10 (Bedrock Guardrails, Model Armor, OpenAI Moderation, multi-layer chain) address this. AAGATE-style "LPCI defenses" go further — semantic pattern analysis at reasoning layer, not just input/output filtering. |
| **ASI-02** | Tool Misuse and Exploitation | `ToolPolicy` CRD with allow/deny/approval rules; AGT policy hook on tool calls; rate-limit per tool; per-tool args schema | **Competitive** | CEL-based RBAC not yet implemented (parity plan #14). No tool poisoning detection — a poisoned MCP tool description that misleads the agent is not detected today. Tool integrity attestation beyond allowlists (sign tool descriptions, not just hosts) — not yet shipped. |
| **ASI-03** | Identity & Privilege Abuse | Workload Identity / Entra Agent ID per-pod; short-lived TokenRequest with 5-min TTL for SRE writer; ServiceAccount per sandbox; no-credential-in-agent-process for upstream | **Best in class** | Microsoft Entra Agent ID first-class integration is ahead of every competitor we surveyed. The "agent has no upstream credential" property is structurally stronger than gateway-style models. |
| **ASI-04** | Memory & Context Poisoning | `KarsMemory` CRD binds the memory store; per-sandbox memory scope; binding lifecycle policy; controller-mirrored binding ConfigMap | **Competitive** | No specific detection of poisoned memory content; no validation of context-input provenance; no "tainted memory propagation" tracking. AAGATE-style information flow tracking would address this. |
| **ASI-05** | Insecure Inter-Agent Communication | **AgentMesh (Signal Protocol, X3DH + Double Ratchet) end-to-end encrypted between every pair of agents**; broker sees only DIDs + ciphertext; KNOCK gate at session establishment; trust scores; per-runtime parity (Python + TypeScript verified on AKS) | **Best in class** | This is the moat. No other agentic-AI platform we surveyed implements Signal-Protocol E2E between agents. AAGATE recommends "Zero-Trust Service Mesh" (mTLS); we go further with forward secrecy + KNOCK + trust progression. **Worth a whole section in any external positioning material.** |
| **ASI-06** | Supply Chain Exposure | Cosign-attested allowlists (egress rules as OCI artifacts); cosign-signed sandbox images; cargo-deny + cargo-audit in CI; SBOM generation; per-PR dependency-review gate | **Competitive** | Tool / MCP server / model image attestation not yet enforced at the router (verify-signature-before-use is partial). MCPSHIELD's "cryptographic attestation of tools" is the next step. No SLSA-level provenance attestation on the agent runtime images today. |
| **ASI-07** | Unexpected Code Execution | **Four-layer defense**: (1) iptables egress-guard locks UID 1000 to loopback + DNS, (2) NetworkPolicy with defaultDeny, (3) per-pod inference-router policy enforcement, (4) AGT governance hook + KNOCK; `seccomp: kars-strict`; `readOnlyRootFilesystem: true`; `runAsNonRoot: true`; `allowPrivilegeEscalation: false`; `capabilities.drop: ALL`; optional `isolation: confidential` (AMD SEV-SNP / Intel TDX) | **Best in class** | The four-layer model is what the CSA Antigravity research note recommends as the minimum bar for IDE-style agents — kars ships it by default. Confidential-VM-per-sandbox is what regulated industries need; we have it as a one-flag flip. Composes cleanly with SIG `Sandbox` + Kata/gVisor RuntimeClass for those who want VM-level isolation on top. |
| **ASI-08** | Cascading Failures | Per-sandbox failure isolation via namespacing; SRE agent (`KarsSREAction`) for autonomous remediation with bounded 5-min CRB; late-recovery healer; spec.suspended graceful pause | **Behind SOTA** | **No delegation chain depth limit** (an agent can spawn arbitrarily deep chains). **No fleet-wide circuit breaker** (one failing tool / model deployment can cascade; we have per-sandbox token budget but no cluster-wide cascade detection). **No "fast kill switch" across the fleet** like AAGATE's GOA. SRE agent recovers known issues; it doesn't prevent cascades from starting. |
| **ASI-09** | Human-Agent Trust Exploitation | Two-step human approval on SRE actions (`KarsSREAction.spec.approval`); operator dashboards; per-action audit | **Behind SOTA** | Two-step approval is only on `KarsSREAction`. For other sandbox spawns / tool calls / sub-agent delegations, there is **no general human-in-the-loop framework**. The NIST Agentic Profile requires autonomy-tier-dependent approval gates; we have neither autonomy tiers nor general approval surfaces. |
| **ASI-10** | Rogue Agents & Behavioral Drift | Per-call OTel; OTel GenAI semantic-convention spans; per-sandbox audit; KarsSandbox status conditions | **Behind SOTA** | **No baseline behavior profiling**, no **anomaly detection on agent action patterns**, no **drift score**. AAGATE's Behavioral Analytics and QSAF (Cognitive Degradation Monitoring) target exactly this. Multi-Agent Security paper highlights stealth attacks specifically rely on the *absence* of behavioral baselines. This is the single biggest gap we have vs the published reference architecture. |

---

## Mapping kars against the NIST AI RMF Agentic Profile

| RMF function | Agentic extension | Kars coverage | Status |
|---|---|---|---|
| **GOVERN** | Formal autonomy tier classification + Agent Owner roles | **None today.** No autonomy taxonomy on `KarsSandbox`; no concept of "autonomy level"; no Agent Owner role | **Gap** |
| **MAP** | Systematic tool-use risk modeling + action-consequence mapping | Partial. `ToolPolicy` denies / allows / requires-approval is a primitive action-consequence map; per-action `KarsSREAction` enforces consequence-aware execution; no formal tool-use risk-tier framework | **Partial** |
| **MEASURE** | Real-time behavioral metrics + autonomy calibration + delegation chain monitoring | Partial. OTel GenAI spans cover per-call telemetry; SRE watcher covers state changes; **no delegation chain depth metric, no autonomy-calibration measurement, no behavioral baseline** | **Partial** |
| **MANAGE** | Incident response (agent compromise) + drift correction + principled decommissioning | Partial. SRE agent handles known recovery patterns; `spec.suspended` enables graceful pause; **no formal "decommissioning" lifecycle that withdraws identities, revokes federated credentials, archives audit, freezes state**; no documented drift-correction playbooks | **Partial** |

---

## Mapping kars against the AAGATE reference architecture

| AAGATE component | Kars equivalent | Status |
|---|---|---|
| **1. Governing-Orchestrator Agent (GOA)** — fleet-wide brain + millisecond kill-switch | **None today.** SRE agent is reactive per-incident; no fleet-wide kill authority | **Gap** |
| **2. ComplianceAgent** — continuous evaluation against OWASP AIVSS + OPA/Rego | Partial. Compile-time policy in controller + cosign attestation; **no continuous AIVSS scoring; no OPA/Rego runtime policy** | **Partial** |
| **3. Behavioral Analytics** — anomaly + drift detection | **None today.** OTel metrics exported but no analytics layer that flags anomalies | **Gap** |
| **4. Zero-Trust Service Mesh** — segmentation + identity | **Better than AAGATE recommends.** AgentMesh provides E2E Signal Protocol secrecy (forward-secret), not just mTLS. NetworkPolicy + identity via WI / Entra Agent ID | **Best in class** |
| **5. Decentralized Accountability** — logging + audit + on-chain | Partial. Per-call OTel, per-CR status conditions, KarsSREAction audit trail. No on-chain integration (likely overkill for most). | **Competitive** |
| **6. LPCI (Logic-Layer Prompt Control Injection) defenses** | Single-layer Prompt Shields today; multi-layer chain in parity plan; no reasoning-layer semantic analysis | **Partial** |
| **7. QSAF (Cognitive Degradation Monitoring)** — detect reasoning drift + recursive confusion | **None today.** | **Gap** |
| **8. DIRF (Digital Identity Rights Framework)** | Partial. Entra Agent ID per-sandbox provides identity; no "agent likeness" replication control surface today | **Partial** |
| **7 continuous control loops** | Partial. SRE watcher is one; per-call rate-limit is two. **No automated adversarial red-teaming loop, no millisecond fleet-wide containment, no automated AIVSS scoring loop, no behavioral drift loop, no compliance posture loop.** | **Partial** |

---

## Mapping against MCPSHIELD defense-in-depth checklist

| MCPSHIELD layer | Kars coverage | Status |
|---|---|---|
| Capability-based access control | `ToolPolicy` + per-sandbox compiled bundle; CEL planned | **Competitive** |
| Cryptographic attestation of tools + data + agents | Cosign-attested allowlists (data); sandbox image signing (agents); **no tool description attestation** | **Partial** |
| Information flow tracking | None today. No "tainted memory" or "data lineage" propagation | **Gap** |
| Runtime policy enforcement | Inference router with compiled policy bundles, hot-reload via ConfigMap watch | **Best in class** (per-pod + cosign-attested distinguishes us) |
| Formal verification of agent-tool interactions | None. No SSA / static analysis on agent-tool flows | **Gap** |

---

## Mapping against multi-agent security threats (arXiv 2505.02077)

| Multi-agent threat | Kars mitigation today | Status |
|---|---|---|
| Secret collusion between agents | TrustGraph (operator-curated) + KNOCK gate at session establishment + per-agent ToolPolicy; **but: no detection of collusive patterns at runtime** | **Partial** |
| Coordinated swarm attacks | Per-agent rate-limit via InferencePolicy; per-peer mesh KNOCK; **but: no swarm-detection at fleet scope** | **Partial** |
| Fast-propagating disinformation | KNOCK + trust scores slow propagation; **but: no content-provenance tracking across mesh hops** | **Partial** |
| Privacy breaches via inter-agent aggregation | Per-sandbox identity + namespace isolation; **but: no cross-agent information flow tracking** | **Partial** |
| Stealth attacks via dispersion | Per-call OTel surfaces individual actions; **but: no cross-agent correlation engine** | **Gap** |

---

## What kars genuinely beats the SOTA reference architectures at

1. **Per-pod egress trust boundary with credentials outside the agent process.** AAGATE's Zero-Trust Mesh recommendation focuses on segmentation; kars's iptables egress-guard + credential-in-sidecar combination is structurally stronger than what AAGATE specifies. Neither MCPSHIELD nor the NIST Agentic Profile prescribes this depth.
2. **E2E encrypted inter-agent communication with Signal Protocol.** AAGATE and NIST recommend "secure inter-agent comms" without prescribing the protocol; AgentMesh's X3DH + Double Ratchet + KNOCK is concretely stronger (forward secrecy, broker-out-of-trust-set).
3. **Cross-runtime mesh interop.** Hermes Python ↔ OpenClaw TypeScript verified on AKS. No published reference architecture handles this; most assume single-runtime fleets.
4. **Confidential-VM sandboxes as a one-flag flip.** Critical for high-regulated industries; AAGATE doesn't require it; SIG `Sandbox` requires operator to wire RuntimeClass; kars makes it default-eligible via `spec.sandbox.isolation`.
5. **Compiled, deterministic, cosign-attested policy bundles.** The combination of byte-deterministic policy compilation + cosign signing + hot-reload + per-sandbox scope is unique to kars among the surveyed platforms.
6. **Microsoft Entra Agent ID first-class integration.** First-class identity primitive purpose-built for agents (GA April 2026); kars has the cleanest integration we found in any open-source agent platform.
7. **Autonomous SRE agent with bounded short-lived RBAC.** The `KarsSREAction` pattern (5-min token + scoped CRB + two-step human approval + late-recovery healer) is a credible NIST MANAGE-function implementation that AAGATE GOA-style fleet-wide brain still has on its roadmap.

These are the seven irreducible advantages that should be prominent in every external positioning artifact — they map directly to SOTA reference-architecture requirements where kars meets or exceeds the bar.

---

## The honest gaps — what would make kars not just competitive but definitively SOTA

Eleven concrete work items grouped by source.

### From the OWASP Agentic Top 10

| | Item | Source | Effort | Priority |
|---|---|---|---|---|
| GAP-1 | **Behavioral baseline + drift detection per sandbox** (anomaly score on action patterns; flag rogue behavior) | ASI-10 + AAGATE Behavioral Analytics | 4–6 weeks | High |
| GAP-2 | **Multi-layered guardrail chain at reasoning layer** (not just input/output filter) — chain Prompt Shields → Moderation → custom semantic checks | ASI-01 + AAGATE LPCI | 2–3 weeks | High |
| GAP-3 | **Delegation chain depth limit + cross-spawn monitoring** (cap depth per sandbox; visualize the tree; per-chain action-cost ceiling) | ASI-08 + NIST MEASURE | 3–4 weeks | High |
| GAP-4 | **Tool poisoning detection** — fetch MCP tool descriptions on registration, attest them, detect mid-flight description drift | ASI-02 + MCPSHIELD attestation | 3–4 weeks | Medium |
| GAP-5 | **General human-in-the-loop framework** beyond KarsSREAction (per-recipe / per-call HITL gates with operator approval surface) | ASI-09 + NIST GOVERN | 2–3 weeks | High |

### From the NIST AI RMF Agentic Profile

| | Item | Source | Effort | Priority |
|---|---|---|---|---|
| GAP-6 | **Autonomy tier classification on `KarsSandbox`** (Tier-0 read-only, Tier-1 single-turn, Tier-2 iterative, Tier-3 autonomous-with-checkpoints, Tier-4 fully autonomous) + per-tier policy defaults | NIST GOVERN extension | 2 weeks | High |
| GAP-7 | **Principled decommissioning lifecycle** (revoke fed creds, archive audit, freeze state, deprovision Entra Agent ID, lock the namespace) | NIST MANAGE extension | 2–3 weeks | Medium |

### From AAGATE

| | Item | Source | Effort | Priority |
|---|---|---|---|---|
| GAP-8 | **Fleet-wide millisecond kill-switch** (cluster-scoped CRD that pauses all sandboxes matching a label selector via `spec.suspended`) | AAGATE GOA | 1 week | Medium |
| GAP-9 | **Continuous compliance evaluation** (runtime OPA/Rego or AGT-policy evaluator that re-scores each sandbox against AIVSS / OWASP AIVSS) | AAGATE ComplianceAgent | 4–6 weeks | Medium |
| GAP-10 | **Cognitive degradation monitoring (QSAF)** — detect reasoning drift, recursive confusion, broken-loop patterns from OTel spans | AAGATE QSAF | 4–6 weeks | Low–Medium |

### From multi-agent security research

| | Item | Source | Effort | Priority |
|---|---|---|---|---|
| GAP-11 | **Cross-agent information flow tracking** — taint propagation across mesh sends; per-message provenance labels; aggregation-risk detection | arXiv 2505.02077 + MCPSHIELD information flow | 6–8 weeks | Medium (long horizon) |

**Subtotal across all eleven gaps: ~33–44 engineer-weeks.** Significant but not insurmountable; comparable in scope to the agentgateway parity plan's must-have list.

---

## Sequencing — which gaps to close first

**Tier 1 (next 6–8 weeks, highest impact + reasonable effort)**

- **GAP-6** Autonomy tier classification (2 weeks). Cheapest, highest-impact regulatory alignment. Required dependency for several other gaps.
- **GAP-2** Multi-layered guardrail chain (2–3 weeks). Already in parity plan #10; agentic angle reinforces priority.
- **GAP-5** Human-in-the-loop framework (2–3 weeks). Required by NIST GOVERN; complements GAP-6.
- **GAP-8** Fleet-wide kill-switch (1 week). One CRD + controller wiring; small effort, large incident-response value.

**Tier 2 (next 8–10 weeks after Tier 1)**

- **GAP-1** Behavioral baseline + drift detection (4–6 weeks). Most demanded by ASI-10 + AAGATE; longest-running but the headline gap.
- **GAP-3** Delegation chain depth limit + monitoring (3–4 weeks). Closes ASI-08; needs the metrics from GAP-1.
- **GAP-4** Tool poisoning detection (3–4 weeks). Closes ASI-02 deeper.
- **GAP-7** Principled decommissioning lifecycle (2–3 weeks). NIST MANAGE.

**Tier 3 (Q1–Q2 2027)**

- **GAP-9** Continuous compliance evaluation (4–6 weeks). AAGATE ComplianceAgent.
- **GAP-10** Cognitive degradation monitoring (4–6 weeks). AAGATE QSAF.
- **GAP-11** Cross-agent information flow tracking (6–8 weeks). Research-grade; multi-agent security frontier.

---

## What this changes about positioning

The lead blog post (`docs/internal/blog/01-kars-in-10-minutes.md`) currently lists **four claims** and **seven irreducible advantages**. The SOTA analysis confirms claims 1, 2, 3, 4 are well-founded — and the seven advantages map cleanly to AAGATE Zero-Trust Mesh + MCPSHIELD attestation + NIST identity + ASI-07 sandboxing. **These claims survive scrutiny against the most authoritative reference architectures.**

But the post does not yet position kars against:
- The OWASP Agentic Top 10 by name
- The NIST AI RMF Agentic Profile
- The AAGATE reference architecture
- The published multi-agent security literature

**Recommended update to the lead blog post**: add a section "How kars maps to the published SOTA frameworks" with the same matrix as above, but condensed to one paragraph per framework + a link to this internal doc. This converts "we say we're SOTA" into "we map to the OWASP, NIST, CSA, and academic SOTA references explicitly".

**Recommended update to the competitive-positioning doc**: add the eleven gaps to the leadership plan as Theme 10 ("close the SOTA framework gaps"), with the sequencing tiers above.

---

## Decision asks

1. **Confirm the gap list is the right framing.** I have flagged 11 concrete gaps. Are there any the team views as deliberately out of scope (e.g., GAP-11 information flow tracking might be considered research-grade and deprioritized indefinitely)?
2. **Confirm Tier 1 priorities** — autonomy tier classification, multi-layer guardrails, HITL framework, kill-switch — are the right 4 items to start with.
3. **Agree to add the SOTA-framework mapping section to the lead blog post**, so external readers can verify kars's posture against the authoritative references.
4. **Agree to publish a public "kars and the OWASP Agentic Top 10" doc** — this is the single highest-credibility piece we could ship for procurement conversations.

---

## Sources (verified 2026-06-15)

- OWASP Top 10 for Agentic Applications 2026 (ASI series) — owasp.org/www-project-genai-security/, owasp.org/www-project-agentic-ai/
- NIST AI RMF Agentic Profile (CSA, 2026-03-27 draft) — labs.cloudsecurityalliance.org/agentic/agentic-nist-ai-rmf-profile-v1/
- AAGATE: A NIST AI RMF-Aligned Governance Platform for Agentic AI (CSA, December 2025) — arXiv 2510.25863
- MCPSHIELD: A Formal Security Framework for MCP-Based AI Agents (Acharya & Gupta, April 2026) — arXiv 2604.05969
- Open Challenges in Multi-Agent Security (May 2025) — arXiv 2505.02077
- Security Threat Modeling for Emerging AI-Agent Protocols — arXiv 2602.11327
- CSA Antigravity Sandbox Escape research note — labs.cloudsecurityalliance.org/wp-content/uploads/2026/04/CSA_research_note_agentic-ide-prompt-injection-sandbox-escape_20260422-csa-styled-1.pdf
- NIST AI RMF 1.0 — nist.gov/itl/ai-risk-management-framework
- NIST AI 600-1 (Generative AI Profile) — 2024
- CSA AI Controls Matrix (243 controls, 18 domains, July 2025)
- OWASP LLM Top 10 — owasp.org/www-project-llm-security-top-10/
