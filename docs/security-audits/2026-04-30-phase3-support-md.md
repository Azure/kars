# Security Audit: phase3(oss) S21 — Add SUPPORT.md

**Finding:** FILE-SUPPORT (no SUPPORT.md in repo root — required for every public Microsoft repo)

**Resolved:** Added `SUPPORT.md` at repository root per Microsoft OSPO template.

## Changes

1. **`SUPPORT.md`** (new)
   - How to file issues: GitHub Issues with templates (bug-report, feature-request, security-report)
   - How to get help: GitHub Discussions (preferred) or Issues with `question` label; best-effort only
   - Scope: AzureClaw bugs/features in scope; downstream (OpenClaw, Foundry, AKS, third-party) out of scope
   - Security issues: Route to SECURITY.md (MSRC)
   - Microsoft Support Policy: Open-source, no SLA, community support

## Verification

- Follows Microsoft OSPO template (https://github.com/microsoft/repo-templates/blob/main/shared/SUPPORT.md)
- Consistent with existing SECURITY.md and CONTRIBUTING.md
- ~80 lines, all substantive (no placeholder text)
- Issue templates verified: bug_report.yml, feature_request.yml, security_report.yml, config.yml

---

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Microsoft OSPO
