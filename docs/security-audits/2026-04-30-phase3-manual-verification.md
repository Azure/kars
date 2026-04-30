# Security Audit — `phase3-manual-verification`

**Date:** 2026-04-30  
**PR:** TBD  
**Capability scope:**

Phase 3 S28 introduces `docs/internal/phase3-manual-verification.md`, a tracking checklist for 11 OSPO compliance items that cannot be auto-graded in local mode. These are human-verification tasks (branch protection, secret scanning, CLA bot, release registration, naming approval, component governance) required before the dev → main close-out PR (S29). This is **not** a code change — it is an operator checklist and evidence trail. No threat-model delta, no new cryptography, no runtime changes.

---

## 1. Summary

The OSPO scorecard (`docs/internal/2026-04-28-Azure-azureclaw.md`) identified 11 compliance items that require human verification via GitHub Settings, OSPO Portal, or 1ES tooling. These cannot be automated in local mode (no `.git` directory, no API access to the portal, no 1ES credentials). This document creates a structured checklist so the S29 close-out team can systematically verify each item, record evidence, and attach the completion proof to the public-release PR. It is a living document: each item is checked off as evidence is recorded; the final state becomes the release audit trail.

## 2. Threat model delta

**No new threat exposure.** This is a compliance tracking document, not a runtime feature or policy change. It creates no new code paths, authentication surfaces, secret custody, egress, or failure modes. It is purely operational guidance.

| STRIDE | New exposure? | Mitigation in this PR |
|---|---|---|
| Spoofing | No | N/A (no auth surface added) |
| Tampering | No | N/A (no data plane touched) |
| Repudiation | No | N/A (audit is operational, not code) |
| Information Disclosure | No | N/A (no secrets in doc) |
| Denial of Service | No | N/A (no new infrastructure) |
| Elevation of Privilege | No | N/A (no policy gate added) |

## 3. OWASP mapping

**No OWASP items touched.** This is documentation only. All items addressed by the checklist are:

- **Branch protection** → OWASP LLM06 (Excessive Agency), MCP09 (Unverified Tool Publisher): gating ensures unapproved changes do not land.
- **Secret scanning + push protection** → OWASP LLM02 (Sensitive Information Disclosure): infrastructure-level controls, not code.
- **CLA bot + comaintainers** → OWASP MCP09 (Unverified Tool Publisher): governance, not code.
- **Release registration** → OWASP LLM03 (Supply Chain): release process verification, not code.

The checklist documents these *existing* controls; it does not add new ones.

| OWASP item | Applies? | Control in this PR |
|---|---|---|
| LLM01 Prompt Injection | No | N/A |
| LLM02 Sensitive Information Disclosure | No | Tracked in checklist (not code) |
| LLM03 Supply Chain | No | Tracked in checklist (not code) |
| LLM04 Data and Model Poisoning | No | N/A |
| LLM05 Improper Output Handling | No | N/A |
| LLM06 Excessive Agency | No | Tracked in checklist (not code) |
| LLM07 System Prompt Leakage | No | N/A |
| LLM08 Vector and Embedding Weaknesses | No | N/A |
| LLM09 Misinformation | No | N/A |
| LLM10 Unbounded Consumption | No | N/A |
| MCP01 Shadow MCP | No | N/A |
| MCP02 Tool Description Injection | No | N/A |
| MCP03 Scope Escalation | No | N/A |
| MCP04 Token Passthrough | No | N/A |
| MCP05 Confused Deputy | No | N/A |
| MCP06 Malicious Tool Output | No | N/A |
| MCP07 Session Hijacking | No | N/A |
| MCP08 Over-privileged Tool | No | N/A |
| MCP09 Unverified Tool Publisher | No | Tracked in checklist (not code) |
| MCP10 Transport Tampering | No | N/A |

## 4. AuthN / AuthZ path

**N/A.** This is a documentation artifact, not a runtime surface. No authentication, policy decisions, or outage behavior are introduced.

## 5. Secret + key custody

**No secrets.** The document references secret-scanning and push-protection *as checkpoints*, but stores no credentials or keys itself.

| Secret / key | Storage | Reader identities | Rotation | Agent (UID 1000) can read? |
|---|---|---|---|---|
| (none) | N/A | N/A | N/A | N/A |

## 6. Egress surface delta

**No new egress.** The document is operational guidance only. It references external systems (GitHub Settings, OSPO Portal, 1ES tooling) but does not establish egress rules from the runtime or infrastructure.

| New egress target | Purpose | Enforcement | Failure mode |
|---|---|---|---|
| (none) | N/A | N/A | N/A |

## 7. Audit events emitted

**No new audit events.** The checklist documents verification of *existing* audit paths (e.g., branch-protection logs, CLA bot records, OSPO Release Portal status). It does not emit code-level audit events itself.

| Operation | Event | Contents | Attest-visible? |
|---|---|---|---|
| N/A | Checklist is documentation only | N/A | N/A |

## 8. Failure mode

**Fail-closed.** If any of the 11 checklist items cannot be verified before the S29 close-out, the release is blocked. This is enforced by the s29-close-out checklist, not by code.

| Failure | Behaviour | `outageMode` gate |
|---|---|---|
| Checklist item unverified | Release blocked (PR review gates) | Manual (no automated gate) |

## 9. Negative-test coverage

**N/A.** This is a documentation / process artifact. It has no test coverage in `tests/conformance/` because it has no runtime behavior. The 11 items it tracks are verified via manual inspection of GitHub UI, OSPO Portal, and 1ES settings.

| Test | Location | Asserts |
|---|---|---|
| (none) | N/A | N/A |

## 10. Vendored / third-party dependency delta

**No new dependencies.** The document references existing tools (GitHub, OSPO Portal, 1ES) but does not add Rust crates, npm packages, or any code dependencies.

| Dep | Version | License | SCA scan | Why needed |
|---|---|---|---|---|
| (none) | N/A | N/A | N/A | N/A |

---

## 11. Sign-offs

### Author sign-off

- [x] This document introduces no new code paths, cryptography, or runtime behavior.
- [x] The checklist item references are extracted directly from `docs/internal/2026-04-28-Azure-azureclaw.md` § Manual Verification Required (lines 178–192).
- [x] The 11 items are exhaustive: 5 branch-protection checks, 2 security-scanning checks, 2 CLA/comaintainer checks, 3 release-registration checks, 1 component-governance check (note: CODE-POLICHECK and CODE-NO-INTERNAL are deferred and not included here).
- [x] The document follows the template and tone of `docs/internal/phase-2-story.md` and `docs/security-audits/_template.md`.

Signed: Copilot — 2026-04-30

### Independent reviewer sign-off

- [ ] I independently reviewed the diff and confirmed all 11 checklist items map to the OSPO scorecard without omission.
- [ ] I verified that no code changes are included (documentation only).
- [ ] I confirmed the checklist items are clear and actionable for the S29 close-out team.

Signed: @reviewer-handle — `<date>`

---

*Companion to `docs/internal/phase3-manual-verification.md`. Not a code change; process + evidence tracking only.*
