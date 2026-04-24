# Security Audit: `phase1/policy-provider-migrate-inference`

**Capability:** migrates the four direct `state.governance.evaluate(...)`
call-sites in `inference-router/src/routes/inference.rs` to the four-seam
[`PolicyDecisionProvider`] trait via a new helper module
`routes/inference_policy.rs`.

**Type:** call-site migration. **Backend is unchanged** — the in-tree
`impl PolicyDecisionProvider for Governance` (landed in
`phase1/policy-provider-in-tree`) calls the same `Governance::evaluate`
function the legacy direct calls used. One evaluator, one verdict path.

## 1. Summary

Before this PR, `routes/inference.rs` had four sites calling
`state.governance.evaluate(sandbox_name, &action, None)` directly:

1. **Chat completions pre-flight** (`/v1/chat/completions`, ~line 192) —
   tool-list policy gate before forwarding to Foundry.
2. **Streaming output post-flight** (~line 680, inside response-stream
   closure) — output guardrail check on each completed stream chunk.
3. **Responses API pre-flight** (`/v1/responses`, ~line 818) — same shape
   as chat completions for the newer Responses API.
4. **Image generation pre-flight** (`/v1/images/generations`, ~line 957) —
   prompt + size-class policy gate before DALL·E forwarding.

Mixed shape made the trait less useful as a contract: a future
`AgtPolicyDecisionProvider` (Phase 1 candidate, AGT-SDK-backed) would
have been bypassed by all four sites. Now they route through the trait.

This PR introduces `routes/inference_policy.rs` exposing:

- `pub enum InferenceDecision { Allow, Deny(String) }`
- `pub async fn check(state, sandbox, action) -> InferenceDecision`

The helper:

- Builds a `PolicyRequest { principal, action, payload_digest, context }`
  from the per-site action and dispatches to
  `state.policy_provider.decide(request).await`.
- Translates trait verdicts into the binary `Allow` / `Deny(reason)`
  shape the inference path needs:
  - `PolicyVerdict::Allow` / `AllowWithLabels` → `Allow`.
  - `PolicyVerdict::Deny { reason }` → `Deny(reason)`.
  - `PolicyVerdict::NeedsApproval { .. }` → `Deny(...)` — out-of-band
    approval flow is not wired for inference at this time (see §4 below).
- Translates `PolicyError::{Unreachable, Internal, Malformed}` via
  `strict_error_reason()` which returns a **constant**
  `"policy backend unavailable"` regardless of the inner error, to
  prevent leaking backend topology, upstream URLs, or auth-error text
  into a 403 response body.

## 2. Threat model delta

**STRIDE delta vs `phase1/policy-provider-in-tree`:**

- **Information Disclosure:** the legacy direct calls already mapped
  `Allow` → permit, anything else → reject with the legacy verdict's
  inner reason. The new helper *narrows* what can leak to the response
  body: `PolicyError` variants now collapse to a single fixed string.
  This is a **net reduction** in information disclosure surface.
- **Denial of Service:** unchanged. `PolicyDecisionProvider::decide` is
  called inline on the request path exactly as `Governance::evaluate`
  was before. No spawn_blocking, no extra await points beyond the trait
  dispatch (which is a single direct call into the in-tree impl).
- **Tampering / Spoofing / Repudiation / Elevation:** no path change.

**Backend equivalence:** the in-tree `impl PolicyDecisionProvider for
Governance` (`providers/policy_impl.rs`) calls `Governance::evaluate`
synchronously, then translates the legacy JSON verdict into a
`PolicyVerdict`. Behaviour at the policy engine, AGT audit, and trust
manager is byte-for-byte identical.

## 3. OWASP mapping

- **OWASP LLM06 — Excessive Agency:** policy gate is the primary
  enforcement point. Migration preserves enforcement; per-site error
  envelopes keep the existing client-facing shape (`policy_violation` /
  `policy_denied` / `content_policy_violation` / `content_filter`).
- **OWASP MCP04 — Tool Definition Poisoning / Tool Misuse:** chat
  completions pre-flight + responses API pre-flight evaluate the
  declared tool-list against `ToolPolicy`. Streaming output post-flight
  evaluates emitted content against output guardrails. Image generation
  evaluates prompt + size-class.
- **CWE-209 — Information Exposure Through Error Message:** addressed
  by `strict_error_reason()` constant string. Two unit tests pin this:
  `strict_error_reason_does_not_echo_inner_message` and
  `strict_error_reason_internal_uniform`.

## 4. AuthN / AuthZ path

Unchanged. Caller authenticates via the inference router's existing
sandbox-token / IMDS workload-identity flow upstream of these sites.
The policy decision itself is identity-aware via
`PolicyRequest { principal, .. }` constructed from `sandbox_name`,
exactly as the legacy `evaluate(sandbox_name, ...)` parameter did.

**Outage behaviour:** Strict-mode fail-closed is the default and only
behaviour at this layer. `PolicyError::Unreachable` ⇒ `Deny`. Future
`CachedRead` / `DegradedDev` modes per `spec.agt.outageMode` (plan §1.3)
are an upstream-of-the-trait concern and out of scope for this PR.

**`NeedsApproval` semantics at inference time:** deferred. The trait
verdict exists for tool-invocation flows that have approval-ticket
plumbing (handoff, MCP tool calls). Inference-time approval would
require returning a 202 with an approval ticket the client can poll —
out of scope; mapped to `Deny("inference requires out-of-band approval")`.

## 5. Secret + key custody

No change. Policy decisions never touch key material. The provider
trait passes only request metadata (principal, action, payload digest,
context) and receives a verdict. Agent (UID 1000) cannot read any
provider state — the trait object lives in the router process (UID 1001).

## 6. Egress surface delta

None. The in-tree `impl PolicyDecisionProvider for Governance` makes
the same in-process function calls the legacy direct path made.

## 7. Audit events emitted

No new audit events introduced by this PR. Each policy decision still
flows through `Governance::evaluate`, which appends to the AGT-backed
`AuditLogger` exactly as before.

## 8. Failure mode

**Fail-closed by default.** Every error variant maps to `Deny`, and the
deny reason returned to the client is the constant
`"policy backend unavailable"` — never the inner error text.

Per-site error envelopes preserved:

- **Chat completions:** `{ error: { message: "Blocked by governance
  policy: {reason}", type: "policy_violation", code: "policy_denied" } }`.
- **Streaming output:** `{ error: { message: "Response blocked by
  output policy", type: "content_policy_violation", code:
  "content_filter" } }` returned as raw `Body::from(json.to_string())`,
  not `Json(…)`, to preserve the raw streaming-body shape.
- **Responses API:** same shape as chat completions.
- **Image generation:** `{ error: { message: "Image generation denied
  by policy: {reason}" } }` — bare envelope, no nested `type`/`code`.

**Lost info from migration:** the legacy `tracing::debug!(... %decision,
"AGT policy evaluated inference")` log at site 1 was dropped. It logged
on the allow path only and `decision` was always "allow" by the time it
executed (the `if !allowed` block had already returned). Tautological;
no operational value lost.

## 9. Negative-test coverage

- `routes::inference_policy::tests::strict_error_reason_does_not_echo_inner_message`
  — asserts the constant string does not contain inner error text.
- `routes::inference_policy::tests::strict_error_reason_internal_uniform`
  — asserts all three `PolicyError` variants collapse to the same
  string.
- Existing integration tests in `routes::inference::tests` (chat,
  responses, streaming, images) remain green and continue to assert
  per-site error envelope shapes.
- `providers::policy_impl::tests` (13 verdict-translation cases) cover
  the underlying trait dispatch.

Future negative-test corpus entries (Phase 1 conformance work):
tampered policy version, malformed verdict JSON. These belong with the
provider impl, not this call-site PR.

## 10. Vendored / third-party dependency delta

None. No new crates or npm packages. No `vendor/` changes.

## 11. Sign-offs

Signed-off-by: GitHub Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
