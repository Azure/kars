# Security Audit — `ap2-route-wiring`

**Date:** 2026-05-02
**PR:** TBD (Phase A of competitive §14.6 closure)
**Author:** @pallakatos
**Independent reviewer:** TBD (router-data-plane scope — see `docs/security-reviewers.md`)
**Capability scope:**
Wires the existing AP2 (Agent Payments Protocol) commerce-mandate kernel
(`inference-router/src/a2a/{ap2.rs, mandate_signing.rs, mandate_trust_store.rs, message_send_ap2.rs}`)
into the live A2A JSON-RPC route. Adds a boot-time mandate-issuer trust-anchor
loader (`inference-router/src/a2a/mandate_trust_loader.rs`) and a
`commerce_required` gate that rejects AP2-free `message/send` requests when
the operator has set `AP2_COMMERCE_REQUIRED=1`. No new external surface; the
data path is the existing `POST /a2a` endpoint.

---

## 1. Summary

Until this change, `routes/a2a.rs::dispatch_request` called `handle_message_send`
unconditionally, ignoring the `metadata.ap2` field on inbound messages. The
AP2 kernel — including mandate signature verification, replay-window checks,
ledger append, and `Ap2Denied` error mapping — was complete (612 LOC,
48 unit tests) but unreachable from the live route. This PR replaces the
direct call with `handle_message_send_with_ap2`, which auto-falls-back to the
plain handler when no `metadata.ap2` is present (zero overhead for non-commerce
traffic) and runs full mandate validation when it is. A `MandateTrustStore`
snapshot is loaded from a JSON file (path in `MANDATE_TRUST_FILE` env, K8s
ConfigMap mount being the canonical source) and held by `A2aRouteState`.
A `commerce_required` boolean (env: `AP2_COMMERCE_REQUIRED`) lets operators
fail-closed when an AP2 mandate is structurally absent, ahead of any signature
or content checks.

## 2. Threat model delta

| STRIDE | New exposure? | Mitigation in this PR |
|---|---|---|
| Spoofing | No new surface; pre-existing JWS verification on mandates is now reachable | EdDSA-only pin enforced by `agent_projection::project_anchors`; duplicate-kid rejection in loader |
| Tampering | Mandate ledger appends now happen on the hot path | `Mutex` held across validate-and-record so check-then-write is atomic per request |
| Repudiation | Reduced — successful commerce dispatches are now recorded | Ledger entries persist mandate hash + timestamp (in-memory; durable backing is Phase D scope) |
| Information Disclosure | None — no new logging of mandate contents | Tracing logs trust-anchor count and file path only, never mandate bodies |
| Denial of Service | A malicious caller could spam invalid mandates to consume CPU on signature verification | Existing global rate limit + token budget paths already gate this; ledger lock contention is bounded (single per-router-instance mutex held only across one validate-and-record) |
| Elevation of Privilege | None — `commerce_required` is fail-closed; default off matches prior behaviour | Ed25519 signing keys remain in K8s Secrets, never reachable by UID 1000 |

## 3. OWASP mapping

| OWASP item | Applies? | Control in this PR |
|---|---|---|
| LLM01 Prompt Injection | No | n/a |
| LLM02 Sensitive Information Disclosure | No | Mandate contents never logged |
| LLM03 Supply Chain | Indirect | Trust anchors loaded from operator-controlled file; EdDSA-only pin prevents `alg=none` downgrade |
| LLM04 Data and Model Poisoning | No | n/a |
| LLM05 Improper Output Handling | No | n/a |
| LLM06 Excessive Agency | Yes | `commerce_required` env gate forces a signed mandate before any agent-driven payment dispatch |
| LLM07 System Prompt Leakage | No | n/a |
| LLM08 Vector and Embedding Weaknesses | No | n/a |
| LLM09 Misinformation | No | n/a |
| LLM10 Unbounded Consumption | No new path | Existing rate limiter still gates the route |
| MCP01 Shadow MCP | No | n/a |
| MCP02 Tool Description Injection | No | n/a |
| MCP03 Scope Escalation | Yes | Mandate `scope` field validated against caller authority by `handle_message_send_with_ap2` |
| MCP04 Token Passthrough | No | n/a |
| MCP05 Confused Deputy | Yes | Mandate-issuer trust store is separate from caller-card trust store; cannot mint mandates by spoofing a peer card |
| MCP06 Malicious Tool Output | No | n/a |
| MCP07 Session Hijacking | No | n/a |
| MCP08 Over-privileged Tool | Yes | Same control as MCP03 |
| MCP09 Unverified Tool Publisher | No | n/a |
| MCP10 Transport Tampering | No | TLS unchanged; signed mandate is end-to-end authenticated regardless of transport |

## 4. AuthN / AuthZ path

- **Caller identity:** A2A peer agent (existing EdDSA-signed agent card)
- **Identity proof (token type, signing algo):** Mandate JWS, EdDSA, kid-bound
- **AGT policy decision point:** `handle_message_send_with_ap2` → `MandateTrustStore::resolve(kid)` → JWS verify → ledger replay check → `commerce.allow_dispatch`
- **Outage behaviour (Strict / CachedRead / DegradedDev):** Strict — invalid signature or missing trust anchor → `Ap2Denied`. Mandate-trust file unreadable at boot → router process exits (existing main.rs error path); never silently degrades to no-trust.
- **Default for prod tenants:** Strict (fail-closed). `AP2_COMMERCE_REQUIRED` defaults to `false` to preserve compatibility with non-commerce A2A peers, but when commerce policy is bound by the controller (Phase G), the env will be set to `true` automatically.

## 5. Secret + key custody

| Secret / key | Storage | Reader identities | Rotation | Agent (UID 1000) can read? |
|---|---|---|---|---|
| Mandate-issuer public keys | K8s ConfigMap (read-only mount) | Router process (UID 1001) | Operator pushes new file → router reload | No — mounted at `/etc/mandate-trust/` (1001:1001 0640) |
| Ed25519 signing key (router-side, mandate response signing) | Pre-existing K8s Secret | Router process (UID 1001) | Existing rotation path | No (existing constraint) |

## 6. Egress surface delta

No new egress targets. The mandate trust file is a local mount.

| New egress target | Purpose | Enforcement | Failure mode |
|---|---|---|---|
| (none) | | | |

## 7. Audit events emitted

| Operation | Event | Contents | Attest-visible? |
|---|---|---|---|
| AP2 mandate accepted on `message/send` | `ap2.mandate.validated` (existing in `message_send_ap2.rs`) | mandate hash, kid, scope, ledger seq | Yes (existing path) |
| AP2 mandate rejected (signature/replay) | `ap2.mandate.denied` | error code, kid attempted | Yes |
| `commerce_required` gate trip (no `metadata.ap2`) | `ap2.commerce.required.denied` (NEW — emitted by `commerce_required_response`) | request id, peer kid (if any) | Yes |

## 8. Failure mode

| Failure | Behaviour | `outageMode` gate |
|---|---|---|
| `MANDATE_TRUST_FILE` unreadable at boot | Router exits non-zero (existing `main.rs` panic path) | n/a (boot-time) |
| `MANDATE_TRUST_FILE` malformed JSON | Router exits non-zero | n/a |
| Invalid mandate signature on hot path | `Ap2Denied` JSON-RPC error | Strict default; no fail-open |
| `AP2_COMMERCE_REQUIRED=1` but `metadata.ap2` absent | `Ap2Denied` with `kind=commerceMandateRequired` | Strict |
| Ledger lock poisoned (panic in another thread) | Recover poisoned guard, continue (best-effort to keep route alive); panic is itself an alert in tracing | n/a (defensive) |

## 9. Negative-test coverage

| Test | Location | Asserts |
|---|---|---|
| `commerce_required_rejects_ap2_free_message_send` | `routes/a2a.rs` (new) | `Ap2Denied` returned, error code matches `A2aErrorCode::Ap2Denied`, `data.kind == "commerceMandateRequired"` |
| `commerce_required_off_by_default_allows_plain_message_send` | `routes/a2a.rs` (new) | Default state lets plain `message/send` through unchanged (regression-guard) |
| `request_has_ap2_metadata_detects_present_and_absent` | `routes/a2a.rs` (new) | Pre-check helper correctly detects both shapes |
| `rejects_duplicate_kid_across_specs` | `a2a/mandate_trust_loader.rs` (new) | Loader fails closed on kid collision (prevents shadow trust anchor) |
| `rejects_unsupported_alg` | `a2a/mandate_trust_loader.rs` (new) | Loader rejects non-EdDSA algorithms (no `alg=none` downgrade) |
| `missing_file_is_io_error`, `malformed_json_is_json_error` | `a2a/mandate_trust_loader.rs` (new) | Boot-time fail-closed on misconfiguration |
| Existing 11 AP2 tests in `message_send_ap2.rs` | unchanged | Hot-path mandate validation already covered |
| E2E `test_ap2_mandate_validation` | `tests/e2e/run.sh` (new in this PR) | End-to-end against kind: signed mandate accepted, bad signature rejected, `commerce_required` without mandate → `Ap2Denied` |

## 10. Vendored / third-party dependency delta

No new dependencies. Loader uses existing `serde_json`, `tracing`, and the
pre-existing AP2 / agent-projection crates from the workspace.

| Dep | Version | License | SCA scan | Why needed (citation) |
|---|---|---|---|---|
| (none new) | | | | |

**Source citations (principle §0.2 #10):**
- AP2 spec: <https://github.com/google-agentic-commerce/AP2>, Apache 2.0,
  commit pin already vendored in `inference-router/src/a2a/ap2.rs` doc comments.
- A2A 1.0 GA spec: <https://github.com/a2aproject/A2A/releases/tag/v1.0.0>
  (released 2026-03-12).

## 11. Sign-offs

### Author sign-off

- [x] I have read principles §0.2 #8, #9, #10 of internal Phase 1 plan.
- [x] The capability contains no pseudo-implementations. Every claimed
      control actually runs on the production code path.
- [x] No custom crypto was added (verified by `ci/no-custom-crypto.sh`).
- [x] Negative tests (Section 9) exist and pass.
- [x] The attestation chain (Section 7) is visible via `kubectl claw attest`
      via the existing AGT AuditLogger sink.

Signed: @pallakatos — 2026-05-02

### Independent reviewer sign-off

- [ ] I independently reviewed the diff, not just this document.
- [ ] I verified negative tests fail without the capability and pass with it.
- [ ] I verified the failure mode (Section 8) is fail-closed by default.
- [ ] For router-data-plane changes, I am on the
      `docs/security-reviewers.md` roster.

Signed: @reviewer-handle — `<date>`
