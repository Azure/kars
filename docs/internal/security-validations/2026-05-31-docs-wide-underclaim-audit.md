# Doc-wide under-claim audit — extends maturity.md review across all of `docs/`

**Date**: 2026-05-31
**Scope**: Every `.md` file under `docs/` (excluding `docs/internal/`), grep-cross-referenced against the actual code in `controller/`, `inference-router/`, `cli/`, `sandbox-images/`, `deploy/`.
**Companion to**: [`2026-05-31-maturity-doc-vs-code.md`](2026-05-31-maturity-doc-vs-code.md) — same methodology, broader surface.

## TL;DR

The same under-claim pattern from `maturity.md` propagates into **six other doc files**:

| File | Under-claims found |
|---|---|
| `docs/maturity.md` | 4 row status downgrades + 9 missing rows (covered separately) |
| `docs/roadmap.md` | 4 entries that have already landed or partially landed |
| `docs/security.md` | 3 entries in the "not yet enforced" callout that are now enforced |
| `docs/compliance.md` | Inherits the maturity.md under-claims — needs new rows added to NIST families |
| `docs/api/crd-reference.md` | 2 status notes that are now wrong (TrustGraph caveats accurate; EntraAgentIdentity outdated) |
| `docs/architecture.md` | 1 TrustGraph caveat accurate; EntraAgentIdentity status missing |
| `docs/use-cases.md` + `use-cases/exec-brief-walkthrough.md` | 2 outdated claims (docker harness "scaffolded", harness "AKS and local-k8s only") |

The doc surface has not caught up with what shipped in the last ~4 weeks: per-sandbox Entra Agent ID (PR #360), docker e2e-harness 9/9 (PR #367), all the work in the security validations PR #368.

---

## A. `docs/roadmap.md` — items that have shipped or partially shipped

### A.1 "In-binary JWS verification" — partially shipped (AP2 path is wired)

> *roadmap.md line 33: "kars_a2a_core::verify_inbound_card is library-complete and unit-tested … The next step is an opt-in axum layer inside the gateway that calls the verifier directly"*

Accurate for the AgentCard verifier specifically. But the **AP2 IntentMandate / PaymentAttempt** branch of the same hardening theme IS wired (see maturity-review §A.4). Suggest splitting:
- `AgentCard JWS verify` axum layer → still roadmap ✅
- `AP2 mandate JWS verify in routes/a2a.rs` → **shipped**, should move to "What ships today"

### A.2 "Egress allowlist authority flip" — already authoritative when opted-in

> *roadmap.md line 41: "Today the inline `allowedEndpoints` field on `KarsSandbox` is the source of truth and the signed artifact (when present) is a parallel check."*

This is incorrect. `controller/src/policy_fetcher.rs:917-1100` shows that when `allowlistRef` is set, the signed artifact IS the authoritative source — inline becomes drift-detection only. The roadmap item should be **rewritten** as: *"Make `allowlistRef` required (no inline fallback)"* or *"Remove the inline `allowedEndpoints` field entirely"*.

### A.3 "Aggregate token budgets" — daily + monthly are shipped

> *roadmap.md line 29: "Today only `tokenBudget.perRequestTokens` is enforced; aggregate counters are accepted on the spec and surfaced in status but not yet metered."*

This is **factually wrong**. `inference-router/src/routes/{inference,chat_completions,anthropic_messages}.rs` all call `check_budget(sandbox_name, policy.daily_tokens, policy.monthly_tokens)`. The router has 641 LOC of `budget.rs` doing UTC-calendar daily + monthly counters with on-disk persistence and HTTP 429 on overrun.

The remaining roadmap item is per-hour windows + explicit `rejectOnExceed` field, neither of which is in the current `TokenBudget` struct.

### A.4 "What ships today" list omits 9 capabilities

The bullet list at the top of roadmap.md (line 7-17) catalogues "the current public surface" but doesn't mention:
1. The 7 ValidatingAdmissionPolicies
2. MCP JWKS registry (per-server signing keys via `MCP_JWKS_DIR`)
3. AP2 mandate verification + replay ledger
4. ToolPolicy commerce/approval/rateLimit blocks
5. SignerPolicy ConfigMap hot-reload
6. EgressApproval merging + digest changes
7. KarsMemory store binding
8. ContentSafetyFloor per-category caps
9. FederationPeer trust-circle declarations

These are the same gaps surfaced in the maturity-review §B.

---

## B. `docs/security.md` — "not yet enforced" callout has stale entries

The blockquote at line 20-25 lists 5 items as "not yet enforced":

```
> - TrustGraph mesh-admission gating — accurate (router doesn't gate on it)
> - A2A AgentCard verification in the gateway — accurate (lib only)
> - Signed-OCI egress allowlists — STALE (now authoritative when opted in)
> - Audit-chain head signing — accurate (detection only, not non-repudiation)
> - attest sign / attest verify — STALE (CLI is 618 LOC of real code)
```

### B.1 "Signed-OCI egress allowlists … advisory today (controller fetches and logs)"

Already in §A.2 above. The signed artifact is **authoritative** today when `allowlistRef` is set (branches 2-4 in policy_fetcher.rs); the inline path is the legacy mode that still permits opt-out.

### B.2 "attest sign / attest verify … scaffolded"

`cli/src/commands/attest.ts` is **618 LOC of working code**. The command emits a deterministic attestation receipt (canonical-JSON spec hash, SSA field owners, policy refs, reconcile trace, baseline diff with exit codes 0/2/3). The roadmap parts are cosign-signing the receipt and embedding an AGT receipt — but the **read surface and the diff-based drift detection are live**.

### B.3 "What we do not defend against" section — missing the macOS Docker Desktop limitation

The validation PR #368 confirmed that on **macOS Docker Desktop**, UID 1000 reads `/run/secrets/azure-openai-key` (Finding #1) due to a Docker Desktop VirtioFS UID-virtualization quirk. This contradicts headline guarantee #1 ("The agent does not see Azure credentials") and should be called out either here or as a footnote on Layer 5.

---

## C. `docs/architecture.md`

### C.1 Two-modes section — `kars dev` macOS caveat missing

Line 25 footnote says: *"In `kars dev` (single-container), agent and router live in the same container with separate UIDs (1000 vs 1001); the router's IMDS-derived token never lands on the agent's filesystem, but a kernel-level container escape would defeat the boundary."*

This is true on Linux Docker but **false on macOS Docker Desktop** — UID 1000 can read the API-key file via `docker exec --user 1000` due to the VirtioFS UID virtualization. Same as B.3 above; needs a callout.

### C.2 TrustGraph reconciler-only block — accurate

Line 213 reads correctly. Confirmed against `inference-router/src/governance/mod.rs:257` which loads the projection but only sets a Prometheus metric label — no edge check.

### C.3 No mention of the per-sandbox Entra Agent ID architecture

Search for "Entra Agent ID" or "agent-identity" returns the docs page link but the architecture diagrams don't include the **entra-auth-sidecar** component (live on AKS, 2 replicas, issues tokens scoped `https://ai.azure.com/.default`). The PR #360 (Phase 6) shipped this 2 days ago; architecture.md hasn't been updated to surface it.

---

## D. `docs/api/crd-reference.md` — outdated status notes

### D.1 Line 684 + 694 — `meshAuthBackend: EntraAgentIdentity (scaffolded; see roadmap)`

This is **stale**. `controller/src/auth_config.rs:173` defines the `EntraAgentIdentity` variant; `controller/src/reconciler/mod.rs:1406-1415` wires it; `sandbox-images/openclaw/entrypoint.sh:163-212` implements the `/v1/mesh-token` flow; `inference-router/src/routes/mesh_token.rs` implements the route on the router side. **All four shipped in PR #360.**

Live AKS evidence (from yesterday's validation):
```
MESH_AUTH_BACKEND=EntraAgentIdentity
MESH_AUTH_AUDIENCE=b712af17-b7f7-419f-a306-b86a607d5a21/.default
PINNED_AGENT_IDENTITY_APP_ID=31b9c8dd-7b23-4d27-bd90-fa6c9fc8a765
```

4 unique per-sandbox Entra app IDs. This is the verified-tier mesh trust documented as roadmap.

The relay-side JWKS verification is the only piece I'm uncertain about — needs verifying against the deployed `agentmesh-relay` image in `deploy/agentmesh-agt.yaml`. The AGT upstream PR #2659 (merged in this session) was the upstream-side patch for it; if the deployed relay image is newer than `~/Private/Repos/agt/...`'s build, it's live too.

### D.2 Line 296 — AgentCard verifier "library-only today"

Accurate — `verify_inbound_card` only appears in test code and re-exports.

### D.3 Line 552 — TrustGraph "reconciler-only"

Accurate. The CRD is projected, loaded, and exposes a Prometheus version-hash metric; no router-side edge check.

### D.4 Missing CRD: nothing about `KarsAuthConfig`

README.md was updated in PR #369 to mention `KarsAuthConfig` as one of two infrastructure CRDs (11 total), but `docs/api/crd-reference.md` still doesn't include a dedicated section for it. The CRD definition is in `controller/src/auth_config.rs` (~520 LOC); it's the cluster-scoped singleton that holds the Entra Agent ID tenant-trust anchor.

---

## E. `docs/use-cases/exec-brief-walkthrough.md` — outdated harness claim

### E.1 Line 418

> *"The reproducible end-to-end harness now runs on **AKS** and **local-k8s** (kind + controller). The `docker` platform is scaffolded in `tools/e2e-harness/platforms/docker.sh` and pending its first 9/9 validation run."*

This is **wrong as of PR #367 (merged 18h ago)**. Docker harness verified 9/9 PASS (run `20260530T155258Z`, 808-word brief, 11 URLs). Sentence should read:

> *"The reproducible end-to-end harness runs on **AKS**, **local-k8s** (kind + controller), and **docker** — all three platforms produce 9/9 PASS scorecards (with the docker MCP check correctly bypassed since dev mode has no McpServer CRD support)."*

---

## F. `docs/use-cases.md`

### F.1 Line 22 — "operator TUI renders for sandbox, policy/peer/memory/eval CRDs, and KarsPairing"

This understates the TUI. `cli/src/commands/operator/` is **1042 LOC** including:
- `actions.ts` — destructive operator actions with confirm prompts (74 LOC)
- `panels_overlay.ts` — modal overlays with key-mapping (148 LOC)
- `fetchers/cluster.ts` — live cluster topology fetch (219 LOC)
- `fetchers/sandboxes.ts` — multi-sandbox status fetch (395 LOC)
- `fetchers/security.ts` — security posture surface (428 LOC)
- `keymap.ts` + `keymap.test.ts` — full keymap (143 LOC)

The TUI surfaces sandbox state, EgressApproval status, TrustGraph version-hash, A2A peer cards, blocklist refresh state, etc. The doc undersells it.

---

## G. Final consolidated under-claim list (extends maturity-review §A and §B)

Combining the maturity-review findings with this doc-wide audit:

### Status changes needed in maturity.md (4)
A.1–A.4 from maturity-review — InferencePolicy aggregate budgets, kars attest, signed-OCI authority flip, AP2 path of AgentCard verify.

### Status changes needed elsewhere (this doc) (8)
| # | File | Change |
|---|---|---|
| C.1 | `docs/security.md` line 20-25 callout | Drop "Signed-OCI advisory" + "attest scaffolded" entries; add macOS Docker Desktop limitation |
| C.2 | `docs/roadmap.md` line 29 | Aggregate token budgets shipped for daily+monthly; only per-hour + `rejectOnExceed` are still roadmap |
| C.3 | `docs/roadmap.md` line 33 | AP2 path of in-binary JWS verify is shipped; AgentCard axum-layer wiring is still roadmap |
| C.4 | `docs/roadmap.md` line 41 | Authority flip already implemented when opted-in; roadmap is now "make `allowlistRef` required" |
| C.5 | `docs/architecture.md` line 25 footnote | macOS Docker Desktop caveat missing |
| C.6 | `docs/architecture.md` (new section) | Add entra-auth-sidecar component to architecture diagrams |
| C.7 | `docs/api/crd-reference.md` line 684,694 | Drop "scaffolded" qualifier on EntraAgentIdentity |
| C.8 | `docs/use-cases/exec-brief-walkthrough.md` line 418 | Update harness sentence to include docker |

### Missing rows / sections (extends maturity-review §B) (10)
B.1–B.9 from maturity-review (admission policies, MCP JWKS, AP2 ledger, ToolPolicy gates, EgressApproval semantics, SignerPolicy reload, FederationPeer trust circle, KarsMemory binding, ContentSafetyFloor caps).

Plus:
| F.1 | `docs/use-cases.md` line 22 + new TUI section | Expand TUI description (1042 LOC, security panel, multi-cluster fetcher) |

### Missing CRD reference page (1)
| D.4 | `docs/api/crd-reference.md` | Add §KarsAuthConfig (cluster-scoped, ~520 LOC controller code, holds Entra tenant-trust anchor) |

---

## H. Recommendations (no fixes executed)

Three suggested PRs in the order I'd land them:

1. **`docs: refresh "what ships today" (PR-quality, low risk)`**
   - Update `docs/maturity.md` per maturity-review §3
   - Update `docs/compliance.md` to add NIST rows that follow from the new ✅ items
   - Update `docs/security.md` "not yet enforced" callout (drop 2 stale entries, add macOS caveat)
   - Update `docs/roadmap.md` 3 entries to reflect what shipped
   - Update `docs/use-cases/exec-brief-walkthrough.md` line 418
   - Update `docs/api/crd-reference.md` line 684,694
   - Update `docs/architecture.md` line 25 footnote
   - ~150 lines of doc edits, no code changes

2. **`docs: architecture diagrams + KarsAuthConfig CRD ref (medium effort)`**
   - Add entra-auth-sidecar to `docs/architecture-diagrams.md`
   - Add `§KarsAuthConfig` to `docs/api/crd-reference.md`
   - Update `docs/architecture.md` to surface Phase 6 Entra Agent ID design
   - ~300 lines

3. **`docs: maturity.md restructure (medium effort, contained)`**
   - Add §Admission-time, §MCP gateway, §AP2 sections to `maturity.md`
   - Expand AGT/ToolPolicy section with commerce/approval/rateLimit rows
   - Expand operator TUI mention in `use-cases.md`
   - ~250 lines

Total scope: 3 doc-only PRs, all reviewable in <30 minutes each. No code changes; no runtime impact.

---

## I. Methodology

Same approach as the maturity-review:
- For "scaffolded" claims: locate the symbol in the source, count non-test LOC.
- For "library-only" claims: search for the symbol's call sites; library-only requires zero non-test callers.
- For "reconciler-only" claims: trace the projected ConfigMap field through to a runtime gate (route handler, middleware, or admission webhook). If the field is loaded but only used to set a metric label, the claim stands.
- For "advisory only" / "parallel check" claims: trace the verification result through to the endpoints actually used at request time.

Cross-checked against live AKS evidence collected in PR #368 ([`2026-05-30-all-platforms-validation.md`](2026-05-30-all-platforms-validation.md)). When a doc claim and the live cluster disagree, the live cluster is treated as ground truth.

Numbers and file references above are pulled from `main` at commit `da4b547`.
