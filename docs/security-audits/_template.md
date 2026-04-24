# Security Audit — `<capability slug>`

**Date:** YYYY-MM-DD
**PR:** #XXXX
**Author:** @github-handle
**Independent reviewer:** @github-handle (from `docs/security-reviewers.md` if
router-data-plane / sandbox-image / admission-policy)
**Capability scope:**
<!-- one paragraph. What is landing? Which files? -->

---

## 1. Summary

<!-- one paragraph. What the capability is in plain language. Not marketing;
technical. -->

## 2. Threat model delta

<!-- Which assets gain new exposure. STRIDE categories touched. Diff against
`docs/threat-model.md` (cite section). Any new trust boundaries. Any weakened
or strengthened existing boundaries. -->

| STRIDE | New exposure? | Mitigation in this PR |
|---|---|---|
| Spoofing | | |
| Tampering | | |
| Repudiation | | |
| Information Disclosure | | |
| Denial of Service | | |
| Elevation of Privilege | | |

## 3. OWASP mapping

<!-- Explicit OWASP LLM Top 10 v2.0 and OWASP MCP Top 10 items this
capability touches. For each, name the chosen control. -->

| OWASP item | Applies? | Control in this PR |
|---|---|---|
| LLM01 Prompt Injection | | |
| LLM02 Sensitive Information Disclosure | | |
| LLM03 Supply Chain | | |
| LLM04 Data and Model Poisoning | | |
| LLM05 Improper Output Handling | | |
| LLM06 Excessive Agency | | |
| LLM07 System Prompt Leakage | | |
| LLM08 Vector and Embedding Weaknesses | | |
| LLM09 Misinformation | | |
| LLM10 Unbounded Consumption | | |
| MCP01 Shadow MCP | | |
| MCP02 Tool Description Injection | | |
| MCP03 Scope Escalation | | |
| MCP04 Token Passthrough | | |
| MCP05 Confused Deputy | | |
| MCP06 Malicious Tool Output | | |
| MCP07 Session Hijacking | | |
| MCP08 Over-privileged Tool | | |
| MCP09 Unverified Tool Publisher | | |
| MCP10 Transport Tampering | | |

## 4. AuthN / AuthZ path

<!-- Who calls this surface? How do they prove identity? What AGT policy
decision gates it? What's the outage behaviour (Strict / CachedRead /
DegradedDev)? -->

- **Caller identity:**
- **Identity proof (token type, signing algo):**
- **AGT policy decision point:**
- **Outage behaviour (Strict / CachedRead / DegradedDev):**
- **Default for prod tenants:** Strict (fail-closed)

## 5. Secret + key custody

<!-- Where do secrets live? Who can read them? Rotation story? Answer
explicitly: can UID 1000 (the agent) read this secret? If yes, justify. -->

| Secret / key | Storage | Reader identities | Rotation | Agent (UID 1000) can read? |
|---|---|---|---|---|

## 6. Egress surface delta

<!-- New outbound destinations (FQDN or IP range). How egress-guard / router
enforces them. DNS or IP pinning where applicable. -->

| New egress target | Purpose | Enforcement | Failure mode |
|---|---|---|---|

## 7. Audit events emitted

<!-- For each operation, what lands in AGT AuditLogger? Receipt id only; no
PII. How does `kubectl claw attest` surface it? -->

| Operation | Event | Contents | Attest-visible? |
|---|---|---|---|

## 8. Failure mode

<!-- Every failure path. Default is fail-closed. Any fail-open path MUST be
justified here and gated by a `spec.outageMode` value. -->

| Failure | Behaviour | `outageMode` gate |
|---|---|---|

## 9. Negative-test coverage

<!-- Pointer to `tests/conformance/` entries (§5.4 of implementation plan).
Each negative case enumerated. -->

| Test | Location | Asserts |
|---|---|---|

## 10. Vendored / third-party dependency delta

<!-- New crates or npm packages. SCA scan result. License. Vendored-patch
audit updated if `vendor/*` touched. -->

| Dep | Version | License | SCA scan | Why needed (citation) |
|---|---|---|---|---|

**Source citations (principle §0.2 #10):**
<!-- URL + commit SHA or version for every external spec / API / protocol
used. -->

## 11. Sign-offs

### Author sign-off

- [ ] I have read principles §0.2 #8, #9, #10 of `docs/implementation-plan.md`.
- [ ] The capability contains no pseudo-implementations. Every claimed
      control actually runs on the production code path.
- [ ] No custom crypto was added (verified by `ci/no-custom-crypto.sh`).
- [ ] Negative tests (Section 9) exist and pass.
- [ ] The attestation chain (Section 7) is visible via `kubectl claw attest`
      or explicitly deferred with a ticket.

Signed: @author-handle  — `<date>`

### Independent reviewer sign-off

- [ ] I independently reviewed the diff, not just this document.
- [ ] I verified negative tests fail without the capability and pass with it.
- [ ] I verified the failure mode (Section 8) is fail-closed by default.
- [ ] For admission / router-data-plane / sandbox-image changes, I am on the
      `docs/security-reviewers.md` roster.

Signed: @reviewer-handle — `<date>`
