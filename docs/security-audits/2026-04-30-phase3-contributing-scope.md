# Security Audit: Phase 3 — External Contributions Scope & Goals

**Date:** 2026-04-30  
**Title:** CONTRIBUTING.md § External Contributions — Scope & Goals  
**Scope:** Documentation; clarifies AzureClaw's external contribution policy per OSPO Business Reviewer Checklist (CONTRIBUTING-ENG-GUIDE).  
**Risk Level:** Low — documentation-only change, no code modifications.

---

## Executive Summary

The OSPO Release Compliance Scorecard (2026-04-28) identified a gap in `CONTRIBUTING.md`: the engineering guide covers internal development and vendor-patching steps but does not articulate the audience, contribution scope, or governance for external contributors.

**Finding:** "CONTRIBUTING covers internal engineering steps and vendor-patch process but does not state the audience or contribution goals (e.g., 'we accept bugfixes and provider plug-ins; we do not accept new transports')."

**Resolution:** Added a new top-level section `## External Contributions — Scope & Goals` to CONTRIBUTING.md (lines 5–71) that:

1. **Defines the audience** for external contributions (AKS operators, SDK adopters, security researchers, plugin vendors)
2. **Lists in-scope contributions** (bug fixes, new MCP servers, new channels/plugins, documentation, tests)
3. **Lists out-of-scope contributions** (new transports, sandbox isolation changes, governance bypass, direct telemetry)
4. **Documents triage cadence** (weekly PR reviews, CLA + CI requirements, best-effort support model)
5. **Specifies ADR + RFC requirements** for architecture-level changes
6. **Cross-links to SECURITY.md** for vulnerability reporting

---

## Change Details

### File Modified: `CONTRIBUTING.md`

- **Line range:** 5–71 (67 new lines inserted)
- **Placement:** Immediately after the introductory paragraph ("This project welcomes contributions and suggestions"), before Quick Start section
- **Subsections:** Audience, In Scope, Out of Scope, Triage Cadence & Response Time, Architecture-Level Changes, Security Disclosures

### Key Content

#### Audience (lines 8–14)

Explicitly names four categories of external contributors:
- Azure / AKS operators
- OpenClaw / Agents SDK / Microsoft Agent Framework users
- Security researchers
- MCP vendors and plugin authors

#### In Scope (lines 16–27)

Welcomes 7 categories of contributions:
1. Bug fixes against documented behavior
2. New MCP servers (conforming to CRD, no sandbox relaxation)
3. New channels (Telegram/Slack/Discord/WhatsApp) or web-search plugins
4. Tier-2 BYO runtime adapters
5. Egress allowlist contributions
6. Documentation improvements
7. Test coverage and performance fixes

#### Out of Scope (lines 29–40)

Clearly rejects 6 categories:
1. New cross-cluster transports (AgentMesh is sole sanctioned transport)
2. Sandbox isolation changes
3. Inference router / governance bypass
4. Direct cloud-side telemetry
5. New top-level CRDs without ADR
6. Vendor patches without upstream-PR attempt

#### Triage Cadence (lines 42–45)

States:
- Weekly PR review cadence
- CLA + CI gate requirements
- Best-effort support model with no SLAs

#### Architecture Changes (lines 47–56)

Requires:
- ADR in `docs/adr/`
- Public RFC issue
- Security audit doc in `docs/security-audits/`
- Slice-train pattern for implementation

#### Security Disclosures (lines 58–61)

Cross-links to SECURITY.md; prohibits public vulnerability issues/PRs.

---

## Compliance Alignment

### OSPO Business Reviewer Checklist

✅ **CONTRIBUTING-ENG-GUIDE** (row 4): *"Strengthen with an 'External contributor goals' paragraph."*

This change **resolves** the finding by:
- Explicitly stating the audience (AKS ops, SDK users, researchers, vendors)
- Defining contribution goals and boundaries
- Referencing the vendor-patch process (pre-existing in CONTRIBUTING) within the external-scope context
- Documenting triage expectations and support model

### Security Impact

**Risk Assessment:** None — documentation change only, no code modifications.

- No changes to control flow, APIs, or deployment behavior
- No introduction of new attack surfaces
- No modification of sandbox isolation, governance, or crypto
- No relaxation of security invariants

### Audience Impact

**Positive:** External contributors now have clear expectations upfront:
- Reduced likelihood of out-of-scope PRs
- Faster triage for in-scope contributions
- Clear signal that security researchers and plugin vendors are welcome
- Explicit guidance on when to file ADRs vs. implementation PRs

**Negative:** None anticipated.

---

## Verification

### CI Validation

Passing checks:
- `bash ci/security-audit-required.sh` ✓ (audit doc present, dual Signed-off-by)
- `bash ci/no-stubs.sh` ✓ (no TODOs, FIXMEs, or placeholders in audit text)

### Manual Checks

- ✓ CONTRIBUTING.md reads naturally and does not break existing structure
- ✓ Section placement (after intro, before Quick Start) matches recommended position
- ✓ Cross-references to SECURITY.md, CONTRIBUTING (vendor patches), and docs/architecture.md are correct
- ✓ Language matches OSPO audit recommendation: "we accept bugfixes and provider plug-ins; we do not accept new transports"

---

## Sign-Off

This audit documents the resolution of OSPO finding **CONTRIBUTING-ENG-GUIDE** and supports the "Approve with conditions" recommendation from the 2026-04-28 scorecard.

Signed-off-by: @AzureClawTeam <azureclawteam@microsoft.com>
Signed-off-by: @pallakatos <pallakatos@github.com>

---

## Appendix — References

- **OSPO Audit:** `docs/internal/2026-04-28-Azure-azureclaw.md`, Business Reviewer Checklist row 4
- **Previous State:** CONTRIBUTING.md lacked external-scope guidance
- **ADR Context:** ADR-0001 (AgentMesh as sole transport); see `docs/adr/0001-*.md` for details
- **Vendor-Patch Process:** Previously documented in CONTRIBUTING.md § "Vendor Patches" (now contextually linked)
- **Security Disclosure Process:** SECURITY.md (updated separately per OSPO compliance fixes)
