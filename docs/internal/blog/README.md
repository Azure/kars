# kars blog series — index

A series of internal-first blog posts explaining kars. The lead post is the high-level summary; each follow-up dives into one architectural surface. Audience is technical: SREs, platform engineers, security folks, and AI-platform peers at Microsoft.

Tone: short paragraphs, no marketing words ("revolutionize", "empower"), real code citations, real trade-offs. Every post should be readable in 8–15 minutes by someone who has never heard of kars.

## Series order

1. **[Announcing kars — a position paper on running agents on Kubernetes](01-kars-in-10-minutes.md)** *(lead post)*
   Part announcement, part position paper. Why we built this instead of using Istio agent gateway / A2A / a serverless function. Where we stand vs. the agent-sandbox SIG. Where AGT fits. Why the router is the right place for governance. Read this before any of the others.

2. **[AgentMesh — Signal Protocol between agents, and why we did this](02-agentmesh-deep-dive.md)**
   Why X3DH + Double Ratchet for inter-agent messaging, what the relay and registry actually see (DIDs and ciphertext, never plaintext), how trust scores progress, and what we contributed back to Microsoft AGT.

3. **[Governance plane — nine CRDs that compose into a policy](03-governance-plane.md)**
   `KarsSandbox` is the unit; `InferencePolicy`, `ToolPolicy`, `EgressApproval`, `TrustGraph`, etc. are the policy axes. How cosign-attested allowlists work. How a policy compiles into a router enforcement bundle.

4. **[The autonomous SRE agent — five minutes of trust per fix](04-autonomous-sre.md)**
   A kars-native agent that detects, diagnoses, proposes, and (with human approval) applies repairs to other agents. The state machine. Why we mint a fresh 5-min token + a one-shot ClusterRoleBinding for every action. Late-recovery healing.

5. **[Multi-runtime — one trust boundary, eight agent frameworks](05-multi-runtime.md)**
   Why kars has eight runtime adapters (OpenClaw, Hermes, Anthropic, MAF, LangGraph, LangGraph-TS, Pydantic AI, OpenAI Agents) on the same router + policy plane. The runtime contract. What changes when a new framework joins.

6. **[Sandbox anatomy — what's inside one agent pod](06-sandbox-anatomy.md)**
   The init container, the agent container, the router sidecar, and how iptables locks the agent to loopback + DNS. The four-layer defense-in-depth model. What an attacker has to bypass to exfiltrate from a sandbox.

7. **[Operator UX — Headlamp plugin, mesh inspector, dashboards](07-operator-ux.md)**
   The Headlamp plugin (SRE Console + embedded Hermes PTY chat), the operator's Cluster Health view, the Grafana dashboards. Why we built this on Headlamp instead of a bespoke React app.

## Conventions

- **Filename:** `NN-slug.md` (zero-padded so they sort).
- **No marketing.** If a word would feel out of place in a Slack #engineering channel, don't use it.
- **Cite real files.** When you say "the controller does X", link `controller/src/path.rs:LINE` so a reader can verify.
- **Show the boring parts.** The interesting story is *why* something is constrained, not what bells and whistles it has.
- **One diagram per post, maximum.** Mermaid only (renders on GitHub + mdBook). If the post needs more diagrams, it needs to be split.
- **Length: 800–1500 words.** Anything longer becomes two posts.

## Status

| # | Slug | Status |
|---|---|---|
| 1 | `01-kars-in-10-minutes.md` | draft (v1) |
| 2 | `02-agentmesh-deep-dive.md` | draft (v1) |
| 3 | `03-governance-plane.md` | draft (v1) |
| 4 | `04-autonomous-sre.md` | draft (v1) |
| 5 | `05-multi-runtime.md` | draft (v1) |
| 6 | `06-sandbox-anatomy.md` | draft (v1) |
| 7 | `07-operator-ux.md` | draft (v1) |
