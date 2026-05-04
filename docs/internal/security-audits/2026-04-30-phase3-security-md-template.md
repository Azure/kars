# Phase3(oss) S25: Adopt microsoft/repo-templates SECURITY.md

**Date:** 2026-04-30  
**Ticket:** FILE-SECURITY  
**Template Version:** V1.0.0  
**Status:** Complete

## Summary

Replaced `SECURITY.md` with the official Microsoft template from `microsoft/repo-templates` to comply with Microsoft OSPO requirements. The template includes the required `<!-- BEGIN MICROSOFT SECURITY.MD V1.0.0 BLOCK -->` markers expected by business reviewers and compliance tooling.

## Changes

### Template Adoption
- Source: https://github.com/microsoft/repo-templates/blob/main/shared/SECURITY.md
- Version: **V1.0.0**
- Markers added: `<!-- BEGIN MICROSOFT SECURITY.MD V1.0.0 BLOCK -->` and `<!-- END MICROSOFT SECURITY.MD BLOCK -->`

### Content Preserved
AzureClaw-specific security documentation was preserved below the template block:
- Detailed reporting instructions
- Supported versions table (0.x alpha with best-effort support)
- Security updates policy

### Content Structure
1. Microsoft official template block (lines 1–14)
2. AzureClaw-specific security info (lines 16+, including Nine defense-in-depth layers, governance details, and E2E encrypted mesh architecture)

## Compliance

✓ Addresses OSPO finding: FILE-SECURITY  
✓ Includes required template marker (V1.0.0)  
✓ Preserves existing AzureClaw security documentation  
✓ CI gates pass: `ci/security-audit-required.sh` and `ci/no-stubs.sh`

## Signed-off-by

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>  
Signed-off-by: Azure OpenClaw Team <azureclaw@microsoft.com>
