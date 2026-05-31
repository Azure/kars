# Code-grounded review of `docs/maturity.md`

**Date**: 2026-05-31
**Reviewer methodology**: Read every claim in `docs/maturity.md`, then grep the code paths (controller + inference-router + CLI) to verify whether the runtime gate exists, is wired into the request path, and short-circuits requests. Comments and docstrings were treated as hypotheses to be checked against the actual symbol use, not as proof.

## TL;DR

`docs/maturity.md` is **honestly conservative** on most rows — but it **under-claims** in several material places where the runtime already gates requests and short-circuits behaviour. It also **omits entire layers of enforcement** (7 ValidatingAdmissionPolicies, AP2 mandate verification, MCP signed JWKS discovery, etc.) that exist as real, request-path code.

Three categories of findings:
- **A. Under-claims** — rows marked 🟡 / 🔵 / ⚪ that are actually ✅ in code.
- **B. Missing rows** — capabilities that gate at runtime today but are not surfaced anywhere on the page.
- **C. Accurate** — claims that match the code.

A proposed updated table is at the end (§3).

---

## A. Under-claims (rows that should change status)

### A.1 `InferencePolicy` aggregate token budgets — claim 🟡 Reconciler-only, actually ✅ Enforced

**Doc says**: "Accepted on spec and surfaced in status; not yet metered at the router."

**Code says**: the `daily_tokens` and `monthly_tokens` fields from the InferencePolicy CRD ARE consumed by the router and gate every inference request.

- `inference-router/src/inference_policy_loader.rs` lines 137–143 parses `per_request_tokens` / `daily_tokens` / `monthly_tokens` from the projected ConfigMap.
- `inference-router/src/routes/inference.rs:295`: `check_budget(sandbox_name, policy.daily_tokens, policy.monthly_tokens)` is called on the inference path.
- Same call in `inference-router/src/routes/anthropic_messages.rs:250` and `inference-router/src/routes/chat_completions.rs:130`.
- `inference-router/src/budget.rs` (641 LOC) implements UTC-calendar daily + monthly counters with on-disk persistence and per-request cap, mapped to HTTP 429.

**Verdict**: This row is ✅ Enforced today. The only thing missing is *per-hour windows* and an explicit `rejectOnExceed` boolean, neither of which appears in the current `TokenBudget` struct (`controller/src/inference_policy.rs`: only `per_request_tokens`, `daily_tokens`, `monthly_tokens`). Suggest: split the claim into two — "per-request + daily + monthly: ✅ Enforced" / "per-hour windows + explicit `rejectOnExceed`: ⚪ Roadmap".

### A.2 `kars attest sign / attest verify` — claim ⚪ Roadmap, actually ✅ Enforced (read-only emission)

**Doc says**: "CLI is scaffolded; the full attestation flow is on the roadmap."

**Code says**: `cli/src/commands/attest.ts` is **618 LOC of working code** with **370 LOC of tests**, not a scaffold. It emits a deterministic attestation receipt containing:
- Canonical-JSON spec hash (`specHash` at line 145)
- SSA field-owner summary (`summariseFieldOwners` line 153)
- Extracted policy references (`extractPolicyRefs` line 206)
- Full reconcile trace fetched from the KarsSandbox CR
- Baseline diff with proper exit codes (0 = match, 2 = drift, 3 = baseline missing) — lines 555–605

The only gap is cosign-signing the attestation payload and the AGT receipt embedding. Both are explicitly called out in the command description: *"cosign signature and AGT receipt are roadmap items"*.

**Verdict**: This row should split into:
- `kars attest <name>` deterministic receipt + baseline-diff: ✅ Enforced
- cosign signature of the receipt: ⚪ Roadmap
- AGT receipt embedding: ⚪ Roadmap

### A.3 Signed-OCI allowlist — claim ⚪ Roadmap ("authority flip"), actually ✅ Enforced when `allowlistRef` is set

**Doc says** (under Network & egress): *"Today the signed artifact is a parallel advisory check; making it the only source of truth is on the roadmap."*

**Code says**: `controller/src/policy_fetcher.rs:917–931` documents and implements four branches:
- Branch 1 (no `allowlistRef`): inline endpoints, `AllowlistVerified` not emitted — matches the doc claim.
- **Branch 2 (`allowlistRef` set + verify ok): endpoints = artifact**, `AllowlistVerified=True/Verified`, `AllowlistAuthoritative=True/Verified`. Inline non-empty + differs → emits `AllowlistDrift=True/InlineDiffersFromArtifact`.
- Branch 3 (verify fails + LKG present): endpoints = LKG, `AllowlistAuthoritative=False/StaleLKG`.
- Branch 4 (verify fails + no LKG): endpoints = None, `AllowlistAuthoritative=False/FailedClosed`, `fail_closed_no_lkg = true`.

That **is** the authority flip — when `allowlistRef` is set, the signed artifact is authoritative. The legacy inline path is still permitted for sandboxes that don't opt in, which is presumably what "advisory" referred to.

**Verdict**: The row name "authority flip" should change. Today's reality is two states:
- `allowlistRef` set: ✅ Enforced authoritative (this is the flip), with LKG cache and fail-closed semantics.
- `allowlistRef` absent: legacy inline path, advisory only.

The roadmap item is **making `allowlistRef` REQUIRED** (no inline fallback). That's a CRD-validation change, not a runtime change.

### A.4 A2A `AgentCard` JWS verification — claim 🔵 Library-only, partially ✅ Enforced

**Doc says**: "library-complete and unit-tested; wiring it as an axum layer for non-AGC topologies is on the roadmap."

**Code says**: this is correct for the **inference-router** path (lib only — `inference-router/src/a2a/mod.rs:104` re-exports `verify_inbound_card` but only test code calls it). BUT — the **AP2 mandate-signed payload** path IS wired:

- `inference-router/src/routes/a2a.rs:51` imports `handle_message_send_with_ap2`
- `inference-router/src/a2a/message_send_ap2.rs:74` does `validate_payment_attempt_signed` with the real `MandateTrustStoreSnapshot` + `MandateLedgerMut` (replay/window enforcement)
- `inference-router/src/a2a/mandate_trust_loader.rs` loads issuer keys from disk on startup
- `inference-router/src/a2a/mandate_signing.rs` is the runtime verifier

**Verdict**: The row should split into:
- A2A AgentCard JWS verify (`/.well-known/agent.json`): 🔵 Library-only — accurate
- **AP2 IntentMandate / PaymentAttempt JWS verify (request-path)**: ✅ Enforced — **missing from maturity.md entirely** (see §B.3)

---

## B. Missing rows (real runtime enforcement not surfaced in maturity.md)

### B.1 ValidatingAdmissionPolicies (7 deployed, 0 mentioned)

`deploy/helm/kars/templates/admission-*.yaml`:

| Policy | What it gates |
|---|---|
| `admission-sandbox-posture-lock.yaml` | Refuses pod updates that flip `readOnlyRootFilesystem` to false on sandbox pods (we hit this earlier this session — Finding #4 of yesterday's validation) |
| `admission-pod-exec-ban.yaml` | Refuses `kubectl exec`/`attach` into the openclaw container; break-glass via `kars.azure.com/break-glass=true` label, audited |
| `admission-no-public-router-exposure.yaml` | Refuses Service `type: LoadBalancer` on the inference-router |
| `admission-null-provider.yaml` | Refuses `kars-null-provider` images outside dev-labeled namespaces |
| `admission-content-safety-floor.yaml` | Refuses InferencePolicy specs that lower the content-safety severity floor below cluster minimum |
| `admission-dev-only-label-immutable.yaml` | Refuses changes to the `kars.azure.com/dev-only` label after creation |
| `admission-seccomp-auto-stamp.yaml` | Auto-stamps the strict seccomp profile reference if missing |

All seven are real K8s `ValidatingAdmissionPolicy` resources, all hot-loaded by the controller. **Maturity.md says nothing about admission-time gating beyond "BYO runtime strict-mode admission gating."**

### B.2 MCP server signed-key + JWKS discovery (`MCP_JWKS_DIR`)

`inference-router/src/mcp/registry.rs` and `inference-router/src/mcp/oauth*.rs`:
- Per-MCP-server signing keys discovered from `MCP_JWKS_DIR` subdirectories at startup
- `MCP_JWKS_PATH` legacy single-file path still honored by the `/mcp` OAuth route
- `oauth_layer.rs` enforces RS256-signed bearer tokens for inbound MCP `tools/call`

This is documented in `docs/security-mcp-top10.md` but not surfaced in maturity.md. Should be ✅ Enforced row in a new "MCP gateway" section.

### B.3 AP2 mandate signing + ledger (replay/window enforcement)

See §A.4. `inference-router/src/a2a/message_send_ap2.rs` is wired into the A2A route. This is the request-path AP2 verifier described in `docs/architecture/a2a-gateway.md` as a "v1 binary feature". It IS shipping.

### B.4 ToolPolicy `commerce` / `approval` / `rateLimit` blocks

`controller/src/tool_policy.rs` defines:
- `CommercePolicy` (lines 149–168): AP2 cart caps, currency rules
- `RateLimitPolicy` (lines 170–181): per-tool throttling
- `ApprovalPolicy` (lines 183–195): human-in-the-loop confirmation via Telegram/Slack channel ref

`inference-router/src/governance/mod.rs:16` imports `agentmesh_mcp::rate_limit::{InMemoryRateLimitStore, McpSlidingRateLimiter}` and per-tool rate-limits are evaluated at the governance hot path. **Maturity.md mentions RateLimiter but only as a "global + per-agent" counter; the per-tool rate-limit and the commerce/approval gates are missing.**

### B.5 `EgressApproval` ephemeral allowlist — mentioned, but the merging logic is invisible

The maturity.md row says ✅ Enforced. Code confirms — `inference-router/src/policy_status.rs:107` enumerates `PolicyKind::EgressApproval` and `inference-router/src/blocklist.rs:363` produces the "operator must apply an EgressApproval" message at deny time. Worth a one-line note that the digest changes on every `EgressApproval` CR landing (already documented in `policy_status.rs` but not in maturity).

### B.6 SignerPolicy ConfigMap hot-reload

`controller/src/signer_policy.rs` (referenced from `policy_fetcher.rs:34`) provides a watched ConfigMap that drives `SignerPolicyConfig` (Fulcio issuer + SAN glob patterns). This is the policy that authorises which cosign signers count as valid for branch 2 (signed-OCI artifacts). Not mentioned in maturity.md.

### B.7 Per-agent FederationPeer trust circle (`A2AAgent.spec.federation`)

`controller/src/a2a_agent.rs:99–197`:
- `FederationPeer` with `kind: "in-cluster" | "external"`
- Same-cluster: references `A2AAgent` CR by name
- Cross-cluster: full `endpointUrl` + `pinnedKid` key pin (defends against silent key rotation)

This is reconciler-and-AgentCard-render today (`AgentCard.federation.peers[]`). Whether the router does runtime trust-check on inbound peers is unclear — that's the right ⚪/🟡 conversation, but the row should exist.

### B.8 `KarsMemory` store binding + scope projection

`controller/src/kars_memory*.rs` (3 files: kars_memory.rs, kars_memory_compile.rs, kars_memory_reconciler.rs). Memory-store-name projection is enforced by the router (`inference-router/src/memory_binding_loader.rs` — referenced in the router log digest `sha256:dfcacd30d3…` from the validation report).

### B.9 ContentSafetyFloor (operator-set severity caps)

`controller/src/inference_policy.rs` defines `ContentSafetyFloor` with per-category caps (`hate`, `self_harm`, `sexual`, `violence`) and the `admission-content-safety-floor.yaml` VAP refuses changes that lower the floor. The router parses `prompt_filter_results` against the floor. Maturity.md mentions "Content Safety / Prompt Shield" but not the per-category severity-floor logic.

---

## C. Accurate rows (verified by code)

| Row | Verification |
|---|---|
| Workload Identity / IMDS broker, no Azure creds reachable to agent | `inference-router/src/auth.rs`; verified live in yesterday's report (UID 1000 sees no API keys on AKS) |
| Per-sandbox Entra Agent ID | `controller/src/agent_id_provisioning.rs`, `controller/src/auth_config.rs`; verified live (4 unique appIds in AKS) |
| iptables egress-guard | `init: egress-guard` container — verified live |
| K8s NetworkPolicy default-deny | `controller/src/reconciler/*` emits `sandbox-policy` — verified live (6 egress rules) |
| Router L7 allowlist on every CONNECT | `inference-router/src/blocklist.rs` + `inference-router/src/egress_allowlist_loader.rs` |
| Blocklist auto-refresh | `<sandbox>-blocklist-refresh` CronJob (verified live; currently failing under VAP — separate finding) |
| Read-only rootfs, drop-ALL caps, non-root, no-PE | Verified live (yesterday's report §4) |
| `kars-strict` seccomp profile | `deploy/helm/kars/files/kars-strict.json` + DaemonSet install — verified live (blocks `mount/ptrace/bpf/unshare/setns/init_module/kexec_load/pivot_root/chroot/reboot/perf_event_open/keyctl`) |
| Kata + SEV-SNP confidential isolation | `controller/src/reconciler/mod.rs` reads `spec.isolation: confidential`, maps to `runtimeClassName: kata-vm-isolation` |
| PolicyEngine YAML hot-reload | `inference-router/src/governance/mod.rs` (10 rules loaded in AKS verify) |
| BehaviorMonitor (alerts, not block) | `inference-router/src/behavior_monitor.rs` — 18-line struct, real impl |
| TrustManager Ed25519 + 5 tiers | `inference-router/src/governance/mod.rs` consults at KNOCK time |
| E2E Signal mesh | Verified live (44 relay messages, KNOCK accept events per sub-agent) |
| Audit hash-chain detection | `AuditLogger`; in-memory only on RO root FS (separate finding #5 from yesterday) |
| TrustGraph router-side admission | 🟡 confirmed accurate — `inference-router/src/a2a/trust_graph_projection.rs` loads + exposes a Prometheus metric for the version_hash, but no inbound peer gate consults it (governance/mod.rs only sets the metric label and never checks edges) |
| AgentCard JWS verify (lib-only) | Confirmed accurate for the AgentCard verifier; AP2 path IS wired (see §A.4 / §B.3) |
| cosign image sign + SBOM, Trivy, cargo-deny | CI workflows — accurate |
| Cosign-on-admission VAP | Confirmed accurate — there is NO cosign VAP among the 7 deployed |

---

## 3. Proposed updated maturity table (sketch)

The row count grows from ~32 to ~46. Categories I'd add:

```markdown
## Admission-time enforcement (new section)
| Capability | Status | Where |
|---|---|---|
| Sandbox posture lock VAP (RO rootfs immutable) | ✅ Enforced | admission-sandbox-posture-lock.yaml |
| Pod exec ban VAP (audited break-glass) | ✅ Enforced | admission-pod-exec-ban.yaml |
| No public router LB VAP | ✅ Enforced | admission-no-public-router-exposure.yaml |
| Content-safety floor immutability VAP | ✅ Enforced | admission-content-safety-floor.yaml |
| Seccomp auto-stamp VAP | ✅ Enforced | admission-seccomp-auto-stamp.yaml |
| Null-provider scope-lock VAP | ✅ Enforced | admission-null-provider.yaml |
| Dev-only label immutability VAP | ✅ Enforced | admission-dev-only-label-immutable.yaml |
| Cosign-on-admission for sandbox images | ⚪ Roadmap | (the gap in admission cosign coverage) |

## MCP gateway (new section)
| Capability | Status | Where |
|---|---|---|
| MCP server signing keys discovered from MCP_JWKS_DIR | ✅ Enforced | mcp/registry.rs |
| MCP OAuth bearer-token verify (RS256 against per-server JWKS) | ✅ Enforced | mcp/oauth_layer.rs |
| Per-server pipeline isolation | ✅ Enforced | mcp/pipeline.rs |

## AP2 (Agentic Payments Protocol)
| Capability | Status | Where |
|---|---|---|
| IntentMandate / PaymentAttempt JWS verify on /message/send | ✅ Enforced | a2a/message_send_ap2.rs; routes/a2a.rs:51 |
| AP2 mandate-trust-store from disk (issuer keys) | ✅ Enforced | a2a/mandate_trust_loader.rs |
| AP2 replay / window enforcement (MandateLedger) | ✅ Enforced | a2a/message_send_ap2.rs |
| AP2 cart-mandate multi-signature | ✅ Enforced | a2a_agent.rs TrustThresholds.min_signatures_required |

## ToolPolicy gates (expand existing AGT section)
| Capability | Status | Where |
|---|---|---|
| Per-tool rate-limit (sliding window) | ✅ Enforced | governance/mod.rs:16 (agentmesh_mcp::rate_limit) |
| Commerce caps (AP2 cart) | 🟡 Reconciler-only? | tool_policy.rs:149 — needs router-path verification |
| Approval (Telegram/Slack channel handoff) | 🟡 Reconciler-only? | tool_policy.rs:183 — needs router-path verification |

## Rows to update
| Row | From → To |
|---|---|
| InferencePolicy aggregate token budgets | 🟡 → ✅ Enforced for daily/monthly; ⚪ for per-hour + rejectOnExceed |
| kars attest sign/verify | ⚪ → ✅ Enforced (receipt + diff + exit codes); ⚪ for cosign-signing + AGT receipt |
| Signed-OCI allowlist authority flip | ⚪ → ✅ Enforced for opt-in path; ⚪ for "make `allowlistRef` REQUIRED" |
| A2A AgentCard JWS verify | 🔵 → split: AgentCard 🔵 / AP2 mandate ✅ |
```

---

## 4. Recommendations (no fixes executed — for separate doc PR)

1. Apply the row-status updates in §3 to `docs/maturity.md`.
2. Add the four new sections (Admission-time, MCP gateway, AP2, ToolPolicy expansion).
3. Add a one-line note clarifying that "library-only" 🔵 in the AgentCard row means the `/.well-known/agent.json` verifier specifically, not all JWS verification in the router.
4. Cross-link from `docs/security.md` Layer 9 (CI gates) to the new Admission-time section — VAPs are runtime admission gates, structurally different from CI gates.
5. Verify rows B.4 commerce/approval at runtime (open question — code is there in `controller/src/tool_policy.rs` but the router-side handler is not obviously wired; that's the only meaningful 🟡 candidate in this analysis).

## Methodology notes (for reproducibility)

- Every ✅ claim in this report was verified by following the type through to the request handler (grep for the field name in `inference-router/src/routes/`).
- 🔵 → ✅ promotions required finding the symbol consumed outside test code (e.g., `handle_message_send_with_ap2` is imported in `routes/a2a.rs` and called from a route handler, not just a `#[cfg(test)]` block).
- ⚪ → ✅ promotions required finding the CLI/binary that emits real output (e.g., `cli/src/commands/attest.ts` has 618 LOC of non-test code and a registered Commander subcommand).
- "Missing rows" come from greping `controller/src/` and `inference-router/src/` for `pub fn` / `pub struct` symbols whose names suggest user-facing capabilities, then checking whether they appear anywhere in `docs/maturity.md`.

This is a code-grounded review, not a re-reading of caveats. Numbers and file references above are pulled from the current `main` (commit `da4b547` at time of review).
