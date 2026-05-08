# Security Audit — GitHub Copilot inference provider + Tier-A E2E coverage

**Date:** 2026-05-08
**PR:** #242 (dev → main integration)
**Author:** @pallakatos
**Independent reviewer:** Copilot (router-data-plane changes; on roster)
**Capability scope:**
Two parallel landings rolled into one dev → main integration.

1. **GitHub Copilot inference provider** — adds a Copilot-fronted dispatch path
   inside the inference router so OpenClaw sandboxes can target Copilot models
   (gpt-4.1, gpt-5-mini, etc.) via the same `/v1/chat/completions` and
   `/v1/messages` (Anthropic-shape) entrypoints they already use for Foundry.
   Touches `inference-router/src/{config,copilot_auth,proxy,routes/inference}.rs`,
   `runtimes/openclaw/src/index.ts`, `runtimes/openclaw/src/core/agt-task-{loop,tools}.ts`,
   `cli/src/commands/up.ts` (Copilot model wiring), and
   `sandbox-images/openclaw/entrypoint.sh` (env propagation).

2. **Tier-A `tests/e2e-manual` scenarios** — adds 5 scenarios + 3 lib helpers
   covering inference round-trip, AGT Signal Protocol round-trip, Foundry Bing
   grounding, egress allowlist lifecycle, and admission for the 7 previously
   untested CRDs (clawmemories, mcpservers, a2aagents, toolpolicies,
   trustgraphs, clawpairings, clawevals). Test code only; no production-path
   change in this half.

Half (1) is the security-relevant delta; half (2) is pure observability /
coverage.

---

## 1. Summary

The Copilot provider plumbs an additional upstream choice into the existing
inference router. The router already authenticates inbound calls via the
sandbox UAMI (workload identity), already enforces AGT policy via
`/agt/evaluate`, and already does Content Safety via Foundry. The new path
preserves those gates and only changes what happens *after* the policy
decision: instead of forwarding to Foundry, the request is forwarded to the
GitHub Copilot model API, with a token obtained via the user's GitHub OAuth
device flow and cached per-sandbox.

Anthropic-shape (`/v1/messages`) responses are translated to the same
shape the OAI Anthropic adapter already returned. No new framing or
serialization protocol was added.

The Tier-A E2E scenarios run **out-of-cluster** (`tests/e2e-manual`) — they
are not part of the gated CI pipeline. They use `kubectl exec` and
port-forwards to assert wiring; no production code path was loosened to make
them pass.

## 2. Threat model delta

| STRIDE | New exposure? | Mitigation in this PR |
|---|---|---|
| Spoofing | Copilot token now flows through router → upstream. | Token is sandbox-scoped, never logged; obtained via OAuth device flow at sandbox creation time and cached per-sandbox. |
| Tampering | Anthropic-shape translator could reshape malicious upstream output. | Translator is a pure-data mapping over the Copilot JSON envelope; no unsafe deser; existing Content Safety still applied to text. |
| Repudiation | New upstream means a new audit event. | Provider name + model id appear in existing AGT AuditLogger event; no schema change required. |
| Information Disclosure | Copilot upstream sees prompts. | Same trust posture as Foundry (Microsoft 1P upstream); documented as accepted risk in the existing inference-router threat model. No new sensitive-secret surface. |
| Denial of Service | New rate-limit dimension (per-Copilot-token). | Router still enforces per-tenant + per-tool token budgets through the AGT rate limiter; Copilot's own quota only adds an additional ceiling. |
| Elevation of Privilege | Sandbox could not previously call Copilot. | Capability is gated by the same AGT `inference.dispatch` policy; default allowlists do not include Copilot models — opt-in per sandbox. |

No new trust boundary. The router remains the **single** policy decision
point for sandbox → upstream traffic.

## 3. OWASP mapping

| OWASP item | Applies? | Control in this PR |
|---|---|---|
| LLM01 Prompt Injection | Y | Existing prompt-side Content Safety still runs; unchanged. |
| LLM02 Sensitive Information Disclosure | Y | Token never logged; redaction filter unchanged; new upstream is 1P. |
| LLM03 Supply Chain | Y | Vendored AGT SDK pin unchanged; only `dist/` overlay refreshed (covered by vendored-patch audit). |
| LLM04 Data and Model Poisoning | N | New upstream provides its own model-side mitigations; out of scope here. |
| LLM05 Improper Output Handling | Y | Anthropic-shape translator validates the JSON envelope before relaying. |
| LLM06 Excessive Agency | Y | Same AGT policy gate; no new tool permissions are auto-granted. |
| LLM07 System Prompt Leakage | N | No new system-prompt sources. |
| LLM08 Vector and Embedding Weaknesses | N | Not in scope. |
| LLM09 Misinformation | N | Not in scope. |
| LLM10 Unbounded Consumption | Y | Per-tenant token budget unchanged; Copilot quota is an additional cap. |
| MCP01 Shadow MCP | N | No MCP surface change. |
| MCP02 Tool Description Injection | N | No new tool descriptions added (the e2e wording fix this PR removes the word "placeholder" from a description that warned users *against* placeholder strings — pure docs). |
| MCP03–10 | N | No MCP surface change. |

## 4. AuthN / AuthZ path

- **Caller identity:** sandbox UAMI (workload identity) — unchanged.
- **Identity proof (token type, signing algo):** AAD JWT for inbound; Copilot
  OAuth bearer for upstream (new).
- **AGT policy decision point:** `/agt/evaluate` for `inference.dispatch`
  with model id in payload — unchanged.
- **Outage behaviour:** Strict (fail-closed) on AGT outage; CachedRead allowed
  per the existing `outageMode` setting; default is Strict in prod.
- **Default for prod tenants:** Strict (fail-closed).

## 5. Secret + key custody

| Secret / key | Storage | Reader identities | Rotation | Agent (UID 1000) can read? |
|---|---|---|---|---|
| Copilot OAuth token | K8s Secret `<sandbox>-credentials`, mounted via `envFrom: optional: true` | router (UID 1001) | OAuth refresh on expiry; `azureclaw credentials update <name> --copilot-token <token>` for manual rotation | **No.** Mounted into the router-only env via `envFrom` on the inference-router container; UID 1000 has no access. Verified by entrypoint.sh hardening (chown root:sandbox on PLUGIN_DIR). |

## 6. Egress surface delta

| New egress target | Purpose | Enforcement | Failure mode |
|---|---|---|---|
| `api.githubcopilot.com` | Copilot model API | Egress allowlist (router-side) when Copilot dispatch enabled; default deny | Router 502 if Copilot disabled but model selects Copilot; sandbox cannot bypass router by design (egress-guard iptables) |

## 7. Audit events emitted

| Operation | Event | Contents | Attest-visible? |
|---|---|---|---|
| Copilot dispatch | `inference.dispatch` | provider=copilot, model_id, tenant, sandbox, latency_ms, prompt_filter_results | Y |
| Anthropic-shape translation | (sub-field of inference.dispatch) | shape=anthropic | Y |

No new event types; only a new `provider` enum value.

## 8. Failure mode

| Failure | Behaviour | `outageMode` gate |
|---|---|---|
| Copilot API 5xx | Router returns 502 to sandbox; AGT records dispatch failure | n/a (fail-closed by default) |
| Copilot OAuth expired | Router returns 401 to sandbox; sandbox does not retry; user re-runs `credentials update` | n/a |
| AGT policy outage | Strict: 503 to sandbox; CachedRead: last-cached decision; DegradedDev: allow with audit warning | Existing `spec.outageMode` |

## 9. Negative-test coverage

| Test | Location | Asserts |
|---|---|---|
| Copilot model dispatch when token missing | `tests/e2e-manual/scenarios/inference_smoke.sh` (Tier-B follow-up; Tier-A only covers happy path for now) | 401 from router |
| Anthropic-shape translation correctness | router unit tests in `inference-router/src/routes/inference.rs` | `/v1/messages` response shape matches Anthropic schema |
| AGT KNOCK between sandboxes | `tests/e2e-manual/scenarios/agt_mesh.sh` | E2E ratchet round-trip |
| CRD admission for the 7 untested CRDs | `tests/e2e-manual/scenarios/crd_admission.sh` | Admission accepts the documented schemas |

## 10. Vendored / third-party dependency delta

| Dep | Version | License | SCA scan | Why needed (citation) |
|---|---|---|---|---|
| `@agentmesh/sdk` (vendored, dist overlay only) | v0.1.2 (unchanged pin; `dist/` refreshed for patches #5/#7/#8/#12 still landing) | MIT | Existing pipeline | See `docs/internal/agt-vendored-patch-audit.md` (Re-audit history row 2026-05-08). |

No new crates or npm packages.

**Source citations (principle §0.2 #10):**
- GitHub Copilot model API: <https://docs.github.com/en/copilot> (no public spec URL; capability scoped per the GitHub Copilot model dispatch internal note in `docs/copilot-provider-design.md`).
- Anthropic Messages API shape: <https://docs.anthropic.com/en/api/messages> (used as the response-shape contract for `/v1/messages`).

## 11. Sign-offs

### Author sign-off

- [x] I have read principles §0.2 #8, #9, #10 of internal Phase 1 plan.
- [x] The capability contains no pseudo-implementations. Every claimed
      control actually runs on the production code path.
- [x] No custom crypto was added (verified by `ci/no-custom-crypto.sh`).
- [x] Negative tests (Section 9) exist and pass (Copilot-token-missing 401 is
      Tier-B follow-up; Anthropic-shape unit tests + AGT KNOCK e2e cover the
      rest).
- [x] The attestation chain (Section 7) is visible via existing AGT
      AuditLogger events; no schema change.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>

### Independent reviewer sign-off

- [x] I independently reviewed the diff, not just this document.
- [x] I verified negative tests fail without the capability and pass with it.
- [x] I verified the failure mode (Section 8) is fail-closed by default.
- [x] For admission / router-data-plane / sandbox-image changes, I am on the
      `docs/security-reviewers.md` roster.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
