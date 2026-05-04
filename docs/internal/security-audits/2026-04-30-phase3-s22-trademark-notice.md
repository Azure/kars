# Security Audit: Phase 3 — Trademark Notice (S22 — FILE-TRADEMARK)

## Overview

This audit documents the addition of a Microsoft Trademark notice to the root `README.md` per OSPO finding **FILE-TRADEMARK** (see `docs/internal/2026-04-28-Azure-azureclaw.md`).

## Finding

**FILE-TRADEMARK:** No "Trademarks" section in `README.md`. Required for every public Microsoft repo.

## Change

Added a canonical `## Trademarks` section to `README.md` immediately following the `## License` section.

**Canonical text** (from `docs/releasing/general/trademarks.md` and per `microsoft/repo-templates`):

```markdown
## Trademarks

This project may contain trademarks or logos for projects, products, or services. Authorized use of Microsoft
trademarks or logos is subject to and must follow
[Microsoft's Trademark & Brand Guidelines](https://www.microsoft.com/en-us/legal/intellectualproperty/trademarks/usage/general).
Use of Microsoft trademarks or logos in modified versions of this project must not cause confusion or imply Microsoft sponsorship.
Any use of third-party trademarks or logos are subject to those third-party's policies.
```

## Threat Model Delta

**None.** This is a documentation-only change. No code, infrastructure, or security controls are affected.

## OWASP Mapping

Not applicable for documentation changes.

## Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
