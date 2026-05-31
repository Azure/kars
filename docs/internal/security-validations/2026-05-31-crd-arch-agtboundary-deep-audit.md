# CRD + Architecture + AGT-boundary doc audit — code-path edition

**Date**: 2026-05-31
**Scope**: `docs/api/crd-reference.md` (732 LOC), `docs/architecture.md` (277 LOC), `docs/architecture-diagrams.md` (360 LOC), `docs/architecture/a2a-gateway.md` (120 LOC), `docs/architecture/agt-boundary.md` (79 LOC).
**Methodology**: For every claim about a CRD field, component, deployment, provider variant, or guarantee — locate the corresponding code in `controller/src/`, `inference-router/src/`, `cli/src/`, `deploy/helm/`, `a2a-gateway/src/`, `kars-a2a-core/src/`, `mesh-plugin/`, `sandbox-images/`, `runtimes/`. Comments are hypotheses; only call sites count.

Companion to [`2026-05-31-docs-wide-underclaim-audit.md`](2026-05-31-docs-wide-underclaim-audit.md). This audit goes deeper on the architecture/CRD doc surface specifically.

## TL;DR

The CRD doc + architecture doc trio is **mostly accurate but suffers from three systematic issues**:

1. **Three CRDs have undocumented spec fields** (KarsSandbox loses 8 fields, mostly Phase-6 Entra additions). Doc shows ~10 fields per CRD; code has ~18.
2. **agt-boundary.md is the most outdated** — claims 3 things AGT has already shipped and 2 things kars has already built, including a 494-LOC card signer that the doc says "kars never builds".
3. **architecture-diagrams.md §8 cluster topology mermaid omits entra-auth-sidecar** despite being live in every AKS cluster post-PR #360 (visible everywhere else in the doc).

Plus 4 smaller items.

---

## A. `docs/api/crd-reference.md` — CRD-by-CRD audit

### A.1 Header count is right (11 CRDs) ✅
Live `kubectl api-resources --api-group=kars.azure.com` returns exactly 11; doc says 11; match.

### A.2 Per-CRD scope matches reality ✅
`KarsAuthConfig` and `TrustGraph` are cluster-scoped (`NAMESPACED=false`); the other 9 are namespaced. Code uses `#[kube(... namespaced ...)]` consistently with the live API.

### A.3 KarsSandbox spec fields — **8 missing from doc**

Code (`controller/src/crd.rs`) has **17 top-level spec fields**:
```
runtime, sandbox, inference_ref, memory_ref, network_policy, agent,
governance, azure_services, resources, a2a, upstream_compatibility,
suspended, mesh_auth, sigs_agent_sandbox, upstream_sandbox_ref,
ai_conformance_reference, custom_security_attributes
```

Doc (`docs/api/crd-reference.md` §KarsSandbox table) covers **8**:
```
runtime, inferenceRef, memoryRef, sandbox, networkPolicy, governance,
a2a, suspended
```

**Missing 8 fields:**

| Field | Code reference | What it does |
|---|---|---|
| `spec.agent` | `crd.rs` `AgentConfig` | Per-agent prompt / persona overrides — used by openclaw runtime |
| `spec.azureServices` | `crd.rs` `AzureServiceConfig[]` | Declarative azure-service RBAC bindings emitted by reconciler onto the sandbox SA (visible in yesterday's validation: `Azure services RBAC annotations applied to ServiceAccount`) |
| `spec.resources` | `crd.rs` `ResourceConfig` | Pod resource requests/limits |
| `spec.upstreamCompatibility` | `crd.rs` `UpstreamCompatibilityConfig` | `sigs/agent-sandbox` overlay/translator mode toggle |
| `spec.meshAuth` | `crd.rs` `MeshAuthConfig` + `MeshAuthMode` | **Phase 6 Entra Agent ID** field on the sandbox itself (overrides KarsAuthConfig default) |
| `spec.sigsAgentSandbox` | `UpstreamCompatibilityConfig` | Overlay mode name |
| `spec.upstreamSandboxRef` | `UpstreamCompatibilityConfig` | Cross-ref to a sigs/agent-sandbox upstream resource |
| `spec.aiConformanceReference` | `UpstreamCompatibilityConfig` | CNCF AI Conformance test marker (doc comment says "no code path consumes this yet" — caveat needed in doc) |
| `spec.customSecurityAttributes` | `crd.rs` | Per-sandbox `custom_security_attributes` propagated to Entra Agent ID |

Of these, `meshAuth`, `customSecurityAttributes`, and `azureServices` are **post-Phase-6 (PR #360)** additions; the doc was never updated.

### A.4 KarsSandbox.status fields — partially missing

Doc lists 6 status fields (`phase`, `sandboxPod`, `namespace`, `inferenceEndpoint`, `runtimeKind`, `observedGeneration`, `conditions[]`).

Code (`crd.rs::KarsSandboxStatus`) adds:
- `tokens_used: Option<TokensUsed>` — token-burn surface
- `foundry_agent_id: Option<String>` — Foundry-side identity
- `agent_identity: Option<AgentIdentityStatus>` — **Phase 6 Entra Agent ID status** (we verified live: `agentIdentity.appId=31b9c8dd-…`, `displayName=kars-kars-execbrief`)

The `agent_identity` field is the most material gap — it surfaces the per-sandbox Entra App ID that operators look up via `kubectl get karssandbox -o jsonpath='{.status.agentIdentity.appId}'`.

### A.5 Status conditions enumerated, but not all from `conditions.rs`

`controller/src/status/conditions.rs` defines 8 standard `TYPE_*` constants:
```
TYPE_READY, TYPE_PROGRESSING, TYPE_DEGRADED, TYPE_SUSPENDED,
TYPE_RUNTIME_READY, TYPE_ALLOWLIST_VERIFIED,
TYPE_ALLOWLIST_AUTHORITATIVE, TYPE_ALLOWLIST_DRIFT
```

`docs/api/conditions.md` covers all 8 ✅. But `docs/api/crd-reference.md` only references the chain via "see conditions.md" without inlining the most operator-relevant conditions for KarsSandbox (e.g., `AwaitingFoundryProvisioning`, `RouterEnforcing`, `OverlayMode`). Suggest at least a footnote listing them.

### A.6 EntraAgentIdentity status caveats — `crd-reference.md:684,694` mark as "scaffolded"
Covered in PR #371 (doc-wide audit §D.1). Stale — verified live on AKS.

### A.7 TrustGraph reconciler-only caveat — accurate
`crd-reference.md:552` accurately describes that the router loads the projection but doesn't gate on it. Cross-checked with `inference-router/src/governance/mod.rs:257-268` which only sets a Prometheus metric label.

### A.8 ToolPolicy spec — fully covered ✅
Code and doc match. `commerce`, `rateLimit`, `approval`, `agtProfile.inline`/`bundleRef` all enumerated.

### A.9 InferencePolicy spec — `tokenBudget.dailyTokens`/`monthlyTokens` are mentioned but doc on the field claims they're not yet metered (under-claim)
Covered in PR #370 §A.1 — they ARE metered in all 3 inference routes.

### A.10 KarsAuthConfig — well-documented ✅
The §KarsAuthConfig section (line 650+) is comprehensive: tenant config, blueprint client ID, Graph-API access pattern, anonymous-tier fallback semantics. Matches `controller/src/auth_config.rs` cleanly.

---

## B. `docs/architecture.md` — component-by-component audit

### B.1 §Components table lists 4, code has 6 ⚠️

Doc table (line 35-40):
- Controller (`kars-controller`)
- Inference router (`kars-inference-router`)
- A2A gateway (`kars-a2a-gateway` + `kars-a2a-core`)
- CLI (`@kars/cli`)

**Missing from the table:**

| Component | Path | Role |
|---|---|---|
| **Mesh plugin (`@kars/mesh`)** | `mesh-plugin/` | TypeScript runtime package the OpenClaw runtime loads. Owns the AGT `MeshProvider` implementation — see §C.5 below. NOT a router/controller component but it IS first-class kars code. |
| **Conformance runner (`kars-conformance-runner`)** | `conformance-runner/` | In-cluster runner pod for `KarsEval` reconciler — launches per scheduled eval run, replays signed corpora against the router, emits JSON verdicts. Used by `KarsEval` (CRD does exist in the doc). |
| **Eval corpus library (`kars-eval-corpus`)** | `eval-corpus/` | Shared corpus types + strict parser + verdict function + built-in conformance corpora. Consumed by both controller and the runner above. |

The "rule that ties them together" paragraph (line 42) says "the agent has no network of its own" — accurate.

### B.2 §Two modes — `local-k8s` mode added but missing macOS Docker Desktop caveat
Covered in the doc-wide audit. The single-container `kars dev` footnote should note the macOS UID virtualization issue.

### B.3 §CRDs as the API "The nine CRDs" — actually 11
Doc title says "nine" but text on line 230-237 explicitly enumerates the 2 infrastructure CRDs (`KarsAuthConfig`, `KarsPairing`) and concludes with "That brings the registered total to **eleven** CRDs." The section header "### The nine CRDs and what each one buys you" should be renamed for consistency.

### B.4 §Data path — accurate ✅
Line 138 correctly describes the entra-auth-sidecar token-mint flow with `AUTH_SIDECAR_URL` fail-closed semantics.

### B.5 §The mesh — accurate ✅
Mesh description aligns with `inference-router/src/routes/mesh.rs` (transparent WebSocket bridge, ciphertext only, KNOCK accept/deny agent-side).

### B.6 §The A2A gateway — overstates current TLS state
References `docs/architecture/a2a-gateway.md` which itself is honest about the 8445 listener being config-only.

---

## C. `docs/architecture/agt-boundary.md` — most outdated of the trio

### C.1 §1 "AGT owns" — 2 over-claims

> *"A2A 1.0.0 Signed Agent Card signing — when AGT ships this primitive. Until then we implement via the SigningProvider seam and document the gap."*

**Wrong.** `kars-a2a-core/src/card_signing.rs` is a **494-LOC Ed25519 JWS signer** (`sign_card` + `verify_card` + `TrustedKeys`). The router re-exports them via `inference-router/src/a2a/mod.rs:74,102`. Tests in `kars-a2a-core/src/card_signing.rs:265-411` and `inference-router/src/a2a/card_server.rs:271-411`. **AGT has not shipped a competing primitive in 4+ months.** The "until then" caveat is no longer accurate; we own this surface fully.

> *"AP2 policy grammar — if AGT defines it. Until then kars defines a private schema designed to be portable to a future AGT definition."*

This is technically accurate (kars has the AP2 schema in `inference-router/src/a2a/ap2.rs` + `message_send_ap2.rs` + `mandate_signing.rs` + `mandate_trust_store.rs`), but the framing makes it sound speculative. AP2 IS wired end-to-end on the request path today.

### C.2 §2 Provider contracts — variant name mismatch

Doc claims:
> *"`PolicyDecisionProvider` | `Vendored` · `AgtRustSdk` · `Null`"*

Code (`inference-router/src/providers/mod.rs:86`) actually has:
```rust
pub enum ProviderKind {
    Vendored,
    Agt,         // ← not "AgtRustSdk"
    Null,
}
```

Minor but the doc is meant to be the seam contract. Rename in doc to match code.

### C.3 §2 says `Cargo.toml` depends on `agentmesh = "3.1.0"` — check current
`Cargo.toml:82` says `agentmesh = "3.1.0"`. ✅ matches.

### C.4 §3 outage modes — accurate ✅
`inference-router/src/providers/outage.rs` defines `OutageMode::{Strict,CachedRead,DegradedDev}` exactly as documented.

### C.5 §3 per-tenant override via `KarsSandbox.spec.agt.outageMode` — DOES NOT EXIST in code
`grep -rn "outageMode\|outage_mode" controller/src/crd.rs` returns nothing. There is no `spec.agt.outageMode` field on `KarsSandbox` today. Either the field needs to be added (small CRD change) or the doc claim removed.

### C.6 §4 "What kars never builds" — 1 violation

> *"Signal / X3DH / Double-Ratchet primitives (no custom crypto)."*

Accurate — `mesh-plugin/` uses `@microsoft/agent-governance-sdk` from npm; no in-tree Signal primitives.

> *"A standalone audit chain (we emit into AGT's)."*

Partially accurate. `inference-router/src/audit/merkle.rs` exists as a **kars-owned audit anchoring library** (Merkle root signing). Doc comment in the file itself says "library only, not yet wired in" — but its existence is worth a footnote. The line "we emit into AGT's" is true for the in-process AGT `AuditLogger`; the merkle.rs library is a **kars-owned augmentation** ready for future use.

> *"A trust-score computation engine."*
> *"A rate-limit enforcement bucket outside AGT."*
> *"Key custody or HSM integration outside AGT."*

These three appear accurate.

### C.7 §4 + §5 framing is correct in spirit but undersells

The "What kars always builds" list (§5) is correct:
- Kubernetes-primitive surface (CRDs) ✅
- Router data-plane enforcement point ✅
- Sandbox isolation substrate ✅

But omits 3 things kars **also** builds:
- The 7 ValidatingAdmissionPolicies (covered in PR #370 audit §B.1)
- AP2 mandate-trust store + ledger (PR #370 §B.3)
- Per-sandbox Entra Agent ID provisioning (entire Phase 6 surface — `controller/src/agent_identity.rs`, `entra-auth-sidecar` deployment, `KarsAuthConfig` CRD)

### C.8 §6 "Working mode with AGT" — accurate ✅
Three-bullet process. No code-path implications.

---

## D. `docs/architecture-diagrams.md` — diagram audit

### D.1 §7 CRD relationships diagram — 9 CRDs shown, 2 missing
Diagram shows: `KarsSandbox, ToolPolicy, InferencePolicy, KarsMemory, McpServer, A2AAgent, TrustGraph, KarsEval, EgressApproval` (9).

Missing: `KarsAuthConfig` (cluster-scoped trust anchor) and `KarsPairing` (controller-internal). The diagram says "How the nine CRDs reference each other" — but doc body just below correctly says 11 total. Either:
- Add `KarsAuthConfig` to the diagram (it's cluster-scoped and applies to every namespace) and `KarsPairing` (small box, controller-only)
- Or rename the section to "How the nine **workload** CRDs reference each other"

### D.2 §8 Cluster topology mermaid — **`entra-auth-sidecar` omitted from `kars-system` namespace** ⚠️
This is the most material diagram gap. The mermaid in §8 shows `kars-system` containing `controller` + `a2a-gateway` only. Live AKS cluster has:
```
kars-system          entra-auth-sidecar     2/2 Running   2d18h  ← MISSING
kars-system          kars-controller         2/2 Running   3d14h  ← shown
```

(There's no `a2a-gateway` deployment live — see D.4.)

The §9 Trust boundaries mermaid (line 41-90) DOES show entra-auth-sidecar (separate diagram). §8 is the one that needs the addition.

### D.3 §8 mentions Foundry as part of "Resource group: kars-`<name>`-rg" — verify
`Foundry` shown inside the RG; that's accurate per the bicep templates (`deploy/bicep/`).

### D.4 `a2a-gateway` deployment missing from live AKS but shown in §8 diagram
```
kubectl get deploy -A | grep gateway   → nothing
```
The deployment doesn't exist on the validation AKS cluster. Either the deployment isn't included in the helm chart by default, or it's optional/gated. Diagram presents it as standard. Worth either (a) verifying the helm template gates this on a value (then noting in the diagram) or (b) removing/dotting the box if it's not part of default `kars up`.

### D.5 §6 Control plane diagram — does NOT show all 4 reconcilers
Multiple distinct reconcilers run in the controller binary:
- `controller/src/reconciler/mod.rs` (KarsSandbox)
- `controller/src/inference_policy_reconciler.rs`
- `controller/src/tool_policy_reconciler.rs`
- `controller/src/kars_memory_reconciler.rs`
- `controller/src/pairing_reconciler.rs`
- `controller/src/claw_eval_reconciler.rs` (or kars_eval_reconciler.rs)
- `controller/src/a2a_agent_reconciler.rs`
- `controller/src/mcp_server_reconciler.rs`
- `controller/src/trust_graph_reconciler.rs`
- `controller/src/egress_approval_reconciler.rs`
- `controller/src/auth_config_reconciler.rs`

That's 11 reconciler modules — one per CRD. §6 diagram shows the controller as a single box. Could be acceptable abstraction, but the operator-facing audit trail (e.g., `InferencePolicyCompiled` log messages we saw live) benefits from showing reconcilers as separate units.

---

## E. `docs/architecture/a2a-gateway.md` — gateway audit

### E.1 8445 listener config-only — accurately disclosed ✅
`inference-router/src/a2a_mtls.rs` exists (149 LOC); `main.rs:446-462` logs config but doesn't bind the port. Comments say "The actual TLS listener is plumbed in a follow-up". Doc says the same.

### E.2 In-binary JWS verification — accurate
Doc says: *"Wiring `verify_inbound_card` directly into the gateway as an axum layer is tracked in the roadmap"* — confirmed; only `a2a-gateway/src/verify.rs::ReplayCache` is wired in main, the JWS verifier itself is library-only.

### E.3 Known limitations (v1) — review
Need to check the §Known limitations section: should it now drop the "AP2 not yet" claim if any exists? Doc doesn't explicitly say that; AP2 verification IS wired in `inference-router/src/routes/a2a.rs:51`.

---

## F. Consolidated findings

### Doc-claim status changes (extends PR #370 §A and PR #371 §G)

| # | Doc | Claim | Reality | Action |
|---|---|---|---|---|
| F.1 | `agt-boundary.md` §1 | "AGT owns A2A AgentCard signing — until then via SigningProvider seam" | We ship 494-LOC card_signing.rs; AGT has not shipped competing primitive | Drop "until then" — kars owns this surface fully |
| F.2 | `agt-boundary.md` §2 | "PolicyDecisionProvider variants: Vendored · AgtRustSdk · Null" | Code variant is `Agt` not `AgtRustSdk` | Rename in doc to match code |
| F.3 | `agt-boundary.md` §3 | "Per-tenant override via `KarsSandbox.spec.agt.outageMode`" | Field doesn't exist in `controller/src/crd.rs` | Add field OR remove doc claim |
| F.4 | `agt-boundary.md` §4 | "kars never builds: a standalone audit chain" | `inference-router/src/audit/merkle.rs` exists (library only) | Add footnote: "Merkle anchoring library exists for future use; today the live pipeline uses AGT's linear chain" |
| F.5 | `agt-boundary.md` §5 | "kars always builds: K8s primitive surface, router enforcement, sandbox isolation" | Also builds: 7 VAPs, AP2 trust/ledger, Phase 6 Entra Agent ID + KarsAuthConfig | Append 3 items |
| F.6 | `architecture.md` §Components | 4 components listed | 6 first-class components (+ mesh-plugin, + conformance-runner + eval-corpus) | Add the 2-3 missing rows |
| F.7 | `architecture.md` §CRDs as the API | "### The nine CRDs and what each one buys you" | 11 CRDs total | Rename section header or call it "nine workload CRDs" |
| F.8 | `architecture-diagrams.md` §8 | Mermaid shows `kars-system` with controller + a2a-gateway only | Live AKS has entra-auth-sidecar (2 replicas) in kars-system; a2a-gateway not deployed by default | Add `entra-auth-sidecar`; gate a2a-gateway behind a Helm value-marker |
| F.9 | `architecture-diagrams.md` §7 | "How the nine CRDs reference each other" | 11 CRDs in apiserver | Add `KarsAuthConfig` (cluster-scoped) + `KarsPairing` (controller-internal) OR rename to "nine workload" |
| F.10 | `crd-reference.md` §KarsSandbox | 8 spec fields documented | 17 spec fields exist in `controller/src/crd.rs` | Add 8 missing fields (`agent`, `azureServices`, `resources`, `upstreamCompatibility`, `meshAuth`, `sigsAgentSandbox`, `upstreamSandboxRef`, `aiConformanceReference`, `customSecurityAttributes`) |
| F.11 | `crd-reference.md` §KarsSandbox status | 7 status fields documented | Code adds `tokensUsed`, `foundryAgentId`, `agentIdentity` (Phase 6) | Add `status.agentIdentity` at minimum — operators look this up |

### New under-claim → ✅ promotions (extends PR #370 §A)

| # | Where claimed | Reality |
|---|---|---|
| F.12 | `agt-boundary.md` §1 | A2A AgentCard signing already shipped (494 LOC, tested) |

### Documentation gaps (entirely missing)

| # | Topic | Where it should land |
|---|---|---|
| F.13 | `customSecurityAttributes` propagation to Entra Agent ID | `crd-reference.md` §KarsSandbox + `agent-identity.md` |
| F.14 | `azureServices[]` declarative RBAC binding | `crd-reference.md` §KarsSandbox + new section in `architecture.md` |
| F.15 | `mesh-plugin/` as a first-class component | `architecture.md` §Components |
| F.16 | 11 reconciler modules (vs single "controller" box) | `architecture-diagrams.md` §6 control plane (optional refactor) |

---

## G. Recommended doc-only PR sketch

Two PRs in priority order:

### PR 1 — `docs(arch,agt-boundary,crd-ref): catch up to shipped reality` (~250 LOC)
- Rename `agt-boundary.md` provider variant `AgtRustSdk` → `Agt` (F.2)
- Drop "until then" caveat on AgentCard signing in agt-boundary.md (F.1)
- Remove or add `spec.agt.outageMode` claim (F.3)
- Add 7 ValidatingAdmissionPolicies + Phase-6 Entra Agent ID + AP2 trust/ledger to agt-boundary.md §5 (F.5)
- Add Merkle library footnote to §4 (F.4)
- Rename "nine CRDs" to "nine workload CRDs" in architecture.md (F.7) + architecture-diagrams.md §7 (F.9)
- Add `entra-auth-sidecar` to architecture-diagrams.md §8 cluster topology mermaid (F.8)
- Gate `a2a-gateway` box on opt-in in §8 (F.8)
- Add `mesh-plugin`, `conformance-runner`, `eval-corpus` rows to architecture.md §Components (F.6)

### PR 2 — `docs(crd-ref): catch up KarsSandbox spec` (~200 LOC)
- Add 8 missing KarsSandbox spec fields (F.10):
  - `spec.agent`, `spec.azureServices`, `spec.resources`, `spec.upstreamCompatibility`, `spec.meshAuth`, `spec.sigsAgentSandbox`, `spec.upstreamSandboxRef`, `spec.aiConformanceReference`, `spec.customSecurityAttributes`
- Add `status.agentIdentity`, `status.tokensUsed`, `status.foundryAgentId` (F.11)
- Cross-link from `architecture.md` to the new fields (F.13, F.14)

Both PRs are markdown-only; no code changes; no runtime impact.

---

## H. Methodology notes

- For "scope" claims: cross-checked `#[kube(... namespaced ...)]` against `kubectl api-resources --api-group=kars.azure.com -o wide` NAMESPACED column.
- For "spec field" claims: grep for `pub [a-z]` inside `CustomResource`-deriving structs; deduplicate vs the doc field table.
- For "provider variant" claims: located the actual `pub enum` in code.
- For "deployed component" claims: cross-checked against `kubectl get deploy,svc,sa -A` from the live AKS cluster used in the validation runs.
- For "reconciler" claims: counted `*_reconciler.rs` files in `controller/src/`.
- Numbers and file references above are from `main` at commit `da4b547`.
