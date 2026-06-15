# What it takes for kars to be competitive with agentgateway

**Companion to** [`competitive-positioning-2026-06.md`](competitive-positioning-2026-06.md).
**Audience:** anyone who needs a one-sitting answer to "where do we stand and what's the work".
**Date:** 2026-06-15.

---

## Framing — "competitive" against what?

Agentgateway (LF-hosted, Solo.io-led) and kars are **not the same product shape**. Agentgateway is a centralized gateway (Kubernetes Gateway API `GatewayClass`); kars is a per-pod trust boundary + multi-runtime adapter framework + governance plane + AgentMesh. You can run both in the same cluster — agentgateway in front of your model fleet, kars in the agent's pod — and that is the right deployment shape in many real systems.

"Competitive with agentgateway" therefore means three different things and they need different work:

1. **Eval-checklist parity** — a serious enterprise evaluator can't dismiss kars on "you don't have X that agentgateway has". This is feature-surface work.
2. **Procurement parity** — a CIO / security board can't dismiss kars on "you don't have the legitimacy / governance / community story that agentgateway has". This is credibility work.
3. **Design-fit articulation** — when an evaluator could pick either, they pick kars for use cases kars is better at. This is documentation + demo work.

You need all three. The list below is what each takes, ordered by ROI.

---

## 1. Eval-checklist parity — the feature surface

The gap analysis from the [comparison matrix](competitive-positioning-2026-06.md#comparison-matrix). Concrete deliverables, with rough estimates.

> **Strategy note (2026-06-15):** The original draft of this plan listed "OpenAI-compatible / Anthropic-compatible front-door endpoint" (steal-from-Orka, match-agentgateway) as item #1–#2. We deliberately removed those after re-examining the strategy. A front-door is a *centralized model gateway for external clients* — that is precisely what agentgateway is for, and adding it to kars would (a) collide with agentgateway in the category where they dominate, (b) contradict our per-pod trust-boundary claim (an external IDE has no egress-guard and is a trusted, not adversarial, caller), and (c) blur our product positioning. **If a customer needs external-IDE-to-cluster traffic with governed credentials, the right composition is agentgateway in front of the cluster + kars inside the cluster for agents.** We document that. We do not reimplement it.
>
> The freed engineering belongs in the *agent-runtime egress* category — making the inference router measurably better than agentgateway's gateway-side enforcement *for agent-originated traffic*. Items 1–2 in the table below are renumbered to reflect this.

### Must-haves (the things evaluators will explicitly check for)

| # | Item | Effort | Owner | Notes |
|---|---|---|---|---|
| 1 | Sub-agent spawn governance hardening: validate target / inherit creds / propagate audit context across spawn chains | ~2 weeks | controller + router | Agent-specific; agentgateway has nothing here because it isn't an agent runtime. Strongest differentiation lever. |
| 2 | Unified per-agent action-cost ledger across model + tool + MCP + mesh + spawn (one budget, one audit trail, all surfaces) | ~2 weeks | router + controller | Agentgateway tracks model calls. We should track *full agent action cost*. This is what agent operators actually want to budget on. |
| 3 | Native Anthropic LLM provider in router (no runtime-adapter dep) | ~1 week | router | We have Anthropic-via-runtime; not first-class on the router |
| 4 | Native AWS Bedrock LLM provider | ~1 week | router | Removes Azure-lock perception |
| 5 | Native Google Gemini / Vertex AI LLM provider | ~1 week | router | Ditto |
| 6 | Native Ollama / vLLM (local model) provider | ~1 week | router | Required for sovereign / airgapped + local-dev |
| 7 | AWS Bedrock Guardrails content-safety module | ~1 week | router | Bedrock customers expect it |
| 8 | Google Model Armor content-safety module | ~1 week | router | GCP customers expect it |
| 9 | OpenAI Moderation API guardrail | ~3 days | router | Easiest to add; pure HTTP call pre/post |
| 10 | Multi-layered guardrail chaining in `InferencePolicy.contentSafety` | ~1 week | controller + router | Today it's single Prompt Shields; needs an ordered chain |
| 11 | Per-API-key virtual keys with budget + cost tracking | ~2 weeks | controller + router | Today we have per-sandbox; agentgateway has per-key |
| 12 | Cost-tracking dashboard in Grafana with per-key + per-sandbox + per-team breakdown | ~3 days | monitoring | Pairs with #11 |
| 13 | MCP federation: one logical `McpServer` exposing N backends | ~2 weeks | controller + router | The "Virtual MCP" pattern |
| 14 | CEL-based authz rules in `ToolPolicy` | ~2 weeks | controller + router | Beyond fixed schemas; CEL is the K8s-standard expression language |
| 15 | OpenAI Realtime API support (voice + bidi streaming) | ~2 weeks | router | Requires WebSocket-aware router routing — agent voice agents are growing fast |
| 16 | Model aliasing + content-based routing (route by request body) | ~1 week | router | Standard egress capability for agents |
| 17 | Model failover with outlier detection | ~1 week | router | Resilience parity |
| 18 | Mesh-aware quality-of-service: per-peer rate-limit + fair-share scheduling, KNOCK-result-aware budget allocation | ~2 weeks | router | Mesh-specific; only kars has this surface |
| 19 | Prompt enrichment + prompt templates + request transformations | ~1-2 weeks | router | Lower priority — these are convenience, not threat-model |
| 20 | MCP auth: JWT validation, Keycloak / OIDC provider support | ~1 week | router | We have basic; agentgateway has broad |

**Subtotal:** ~18–22 engineer-weeks to close every item on this list.

### Should-haves (mature evaluators ask for these)

- WebSocket proxying through the router (#15 covers this); HTTP/2 streaming pass-through.
- xDS-style hot reload of policy bundles (we have ConfigMap watch + reload; agentgateway has xDS).
- OpenTelemetry GenAI semantic-conventions span propagation across upstream calls (we partially do this).
- Load balancing between multiple instances of the same model (Power of Two Choices).
- Rate limiting per token, per user, per IP (not just per token-budget).

**Subtotal:** ~6–10 engineer-weeks.

### Could-haves (long tail, depends on evaluator profile)

- HTTPS / mTLS frontend listeners with auto-cert hot-reload.
- Custom WAF webhook callouts.
- TLS settings configuration via CRDs.
- Argo CD / Flux installation guides.

**Subtotal:** ~3–5 engineer-weeks.

**Total feature parity:** ~27–37 engineer-weeks.

---

## 2. Procurement parity — the credibility surface

These are not features; they are organizational signals that a CIO / security board / Architecture Review Board looks for.

| # | Item | Effort | Notes |
|---|---|---|---|
| C1 | v1 API stability commitment | ~4–6 weeks | Required gate for everything else here; involves CRD schema freeze, deprecation policy, conversion-webhook strategy |
| C2 | CNCF Sandbox application | ~4 weeks (after C1) | Application + governance + responses; takes 1–3 months to land |
| C3 | Multi-vendor governance | ongoing | Need at least 2 contributing orgs beyond Microsoft. Realistic targets: Anthropic (already in agent-sandbox SIG), Solo.io (would prefer kars over building competing per-pod product), Confluent / Snowflake / banks |
| C4 | LF home (alternative to CNCF if more appropriate) | ~6 weeks | If we partner with agentgateway side, LF could host us as a sister project |
| C5 | Public design-doc cadence under `docs/design/` | ongoing | Three to publish first: AgentMesh wire format, KarsSandbox v1 schema, SRE action lifecycle |
| C6 | Public roadmap (`docs/roadmap.md`) | 1 week | We have one internally; make it public, update quarterly |
| C7 | Public security policy + responsible disclosure | 1 day | We have SECURITY.md; verify it points to MSRC + has private-disclosure path |
| C8 | Production reference customers named publicly | requires negotiation | Need 2–3 named customers (or convincing case studies) |
| C9 | At least 3 non-Microsoft regular contributors in 6 months | ongoing | Identify likely targets via the AGT and agent-sandbox SIG contributor lists; lower the contribution bar |
| C10 | Apache 2.0 (we're MIT today) | 1 week | Optional; matters more for some procurement processes. Investigate org policy first |
| C11 | Public benchmark publication | ~2 weeks | Following the agent-sandbox SIG model — publish methodology + measured numbers for sandbox cold-start, router latency, mesh frame throughput |

**Total credibility parity:** ~12–16 engineer-weeks of work + ~6+ months of relationship work.

---

## 3. Design-fit articulation — being the obvious choice

This is the cheapest category of work and the highest-ROI for sales / showcase moments. Most of it is docs + demos.

| # | Item | Effort | Notes |
|---|---|---|---|
| D1 | Doc: "kars vs agentgateway: when to use which" | 1 week | Single most useful doc for stakeholder conversations. Lives in `docs/` (public). |
| D2 | Doc + working example: kars sandboxes calling models via agentgateway in front | 1 week | The composition story. Shows we cooperate, not compete |
| D3 | Doc + working example: kars on top of `agent-sandbox` SIG overlay mode end-to-end | 1 week | The SIG alignment story |
| D4 | Demo: multi-runtime app — Hermes + OpenClaw + MAF talking on AgentMesh | 1 week | The only-kars-has-this story |
| D5 | Demo: cross-cluster federated mesh | 2 weeks | The sovereign / B2B story |
| D6 | Demo: confidential-compute sandboxes (AMD SEV-SNP / Intel TDX) | 1 week | The compliance-deal story |
| D7 | Showcase deck refresh with the comparison matrix | 3 days | For practitioner-grade and analyst conversations |
| D8 | Position paper publication on Microsoft Tech Community or Open Source blog | 1 week | Distribution beyond GitHub readers |

**Total articulation work:** ~10 engineer-weeks.

---

## Sequencing — what to do in what order

### Phase 1 (next 6–8 weeks, Q3 2026 early)

**Goal:** close the most-visible eval gaps + ship the composition story.

- **#1, #2** (OpenAI + Anthropic front door) — biggest UX win, smallest effort.
- **#3, #4** (Anthropic + Bedrock native providers) — breaks Azure-lock perception.
- **#7, #9** (Bedrock Guardrails + OpenAI Moderation) — easiest two guardrail integrations.
- **D1** (vs-agentgateway doc) — sales-critical immediately.
- **D2** (kars-behind-agentgateway example) — proves composition.
- **C6** (publish roadmap) — costs nothing, signals seriousness.

**Engineering cost:** ~6 weeks of focused router work + ~1 week of docs.

### Phase 2 (next 8–10 weeks, Q3 → Q4 2026)

**Goal:** reach broad surface parity + start the credibility flywheel.

- **#5, #6, #8** (Gemini/Vertex + Ollama/vLLM + Model Armor) — finishes the provider/guardrail matrix
- **#10, #11, #12** (multi-layer guardrails + virtual keys + cost dashboard) — agentgateway's distinctive features
- **#13** (MCP federation) — closes an obvious gap
- **D3, D4** (SIG-overlay demo + multi-runtime demo) — showcase moments
- **C1** start (v1 API stability work) — foundational for everything that follows
- **C5** (3 design docs published)

**Engineering cost:** ~10 weeks router + controller + ~2 weeks docs + ~3 weeks v1 prep.

### Phase 3 (Q4 2026 → Q1 2027)

**Goal:** procurement-grade legitimacy + tail features.

- **#14, #15, #16, #17** (CEL RBAC, Realtime, aliasing, failover) — feature tail
- **C1** finish + **C2** (v1 release + CNCF Sandbox application)
- **C3** (multi-vendor governance — start recruiting)
- **C8** (production references negotiated)
- **C11** (public benchmarks)
- **D5, D6** (federated mesh + confidential-compute demos)

**Engineering cost:** ~8 weeks router/controller + ~6 weeks v1 + CNCF prep.

---

## Realistic totals + dependencies

| Bucket | Engineer-weeks | Calendar (1 FTE) | Calendar (2 FTEs) | Calendar (4 FTEs) |
|---|---|---|---|---|
| Eval-checklist parity (must-haves) | 18–22 | ~5 months | ~3 months | ~6 weeks |
| Eval-checklist parity (should-haves) | 6–10 | +2 months | +1 month | +2 weeks |
| Credibility parity (work) | 12–16 | ~4 months | ~2 months | ~5 weeks |
| Credibility parity (relationships) | n/a | 6+ months | 6+ months | 6+ months |
| Design-fit articulation | 10 | ~2.5 months | ~5 weeks | ~3 weeks |

**Concrete answer:** ~46–58 engineer-weeks of code + docs work, on top of 6+ months of community / relationship work that runs in parallel and can't be sped up by adding engineers.

At **1 FTE focused**, surface parity in ~11 months; full competitive footing in ~14 months.
At **2 FTEs**, surface parity in ~6 months; full competitive footing in ~9 months.
At **4 FTEs**, surface parity in ~3 months; full competitive footing in ~6 months (community track still gates this).

---

## What we should NOT do

- **Don't try to be a better centralized gateway.** Agentgateway has 9 backers and a year-head-start in that category. We'd be a worse second.
- **Don't fork agentgateway.** They're LF-hosted and Microsoft is an explicit backer; the politics are bad and the technical case is weak.
- **Don't drop the per-pod sidecar trust boundary in favor of a centralized model.** That's the moat. The eval-parity work is on the centralized-style features the *router* needs to add, not on changing where the router runs.
- **Don't try to displace the SIG `Sandbox` primitive.** Compose on top via overlay mode.

## What we should explicitly do that agentgateway cannot

These are non-roadmap items — they exist today and are kars's irreducible advantages. Reinforce, don't dilute.

1. **Per-pod trust boundary** with iptables egress confinement and no upstream credential in the agent process.
2. **E2E encrypted inter-agent mesh** (Signal Protocol, X3DH, Double Ratchet, KNOCK gate, trust scores) — broker sees only DIDs + ciphertext.
3. **Multi-runtime adapter framework** (8 frameworks behind one trust boundary, contract documented).
4. **Cross-runtime mesh interop** (verified Hermes Python ↔ OpenClaw TypeScript on AKS).
5. **Cosign-attested policy bundles** with deterministic byte layout for supply-chain proof.
6. **Confidential-VM sandbox isolation** (AMD SEV-SNP / Intel TDX) as a one-flag flip.
7. **Microsoft Entra Agent ID** as a first-class identity mode (`KarsAuthConfig`).

## Decision points the team should have before committing

1. **Are we resourced to do this?** ~46–58 engineer-weeks is real. At 1 FTE, it's a year; at 2 FTEs, it's 6 months. Below 1 FTE, surface parity slips beyond agentgateway's next 2–3 releases — and we fall further behind, not closer.
2. **Are we OK with composing rather than competing in the gateway category?** This plan assumes yes. If the strategic view is "kars must be the gateway too", the plan is different (and much harder).
3. **Are we willing to invest in community / OSS legitimacy?** Without that, technical parity won't translate to procurement wins.
4. **Do we have a credible v1 API stability story for Q4 2026?** Without it, CNCF Sandbox is off the table.
